// Which TURN server a new session uses: the ManyDesk relay (default), the user's own
// Cloudflare Realtime TURN key, or the manual entry. The first two give short-lived
// credentials, fetched here and cached in settings.
use std::time::Duration;

use serde_json::{json, Value};

use crate::mdp::turn::Server;
use crate::store::{CfCreds, TurnCfg};

/// The ManyDesk relay (turn-worker/): hands out Cloudflare TURN credentials, so its key's
/// secret never ships in the app.
pub const RELAY: &str = "https://manydesk-turn.rzayevsukur.workers.dev/v1/ice";

/// The plain-UDP `turn:` addresses of a list, in its order.
fn udp_urls(urls: &[String]) -> Vec<String> {
    urls.iter().map(|u| u.trim()).filter(|u| u.starts_with("turn:") && !u.contains("transport=tcp")).map(String::from).collect()
}

/// The first plain-UDP `turn:` address of a list.
fn udp_url(urls: &[String]) -> Option<String> {
    udp_urls(urls).into_iter().next()
}

/// Fresh Cloudflare credentials (24 h). Errors are i18n keys ("cf.auth", …).
pub async fn cloudflare(key_id: &str, token: &str) -> Result<CfCreds, (String, u16)> {
    let valid = (8..=128).contains(&key_id.len()) && key_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid || token.trim().is_empty() {
        return Err(("cf.badInput".into(), 0));
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().map_err(|_| ("cf.net".to_string(), 0))?;
    let url = format!("https://rtc.live.cloudflare.com/v1/turn/keys/{key_id}/credentials/generate-ice-servers");
    let res = client.post(url).bearer_auth(token.trim()).json(&json!({ "ttl": 86400 })).send().await.map_err(|_| ("cf.net".to_string(), 0))?;
    let status = res.status().as_u16();
    if !res.status().is_success() {
        let code = match status {
            401 | 403 => "cf.auth",
            404 => "cf.notFound",
            _ => "cf.http",
        };
        return Err((code.into(), status));
    }
    let data: Value = res.json().await.unwrap_or(Value::Null);
    ice_creds(&data, key_id, "cf.noTurn")
}

/// Fresh credentials from the ManyDesk relay (24 h). Errors are i18n keys ("md.net", …).
pub async fn manydesk() -> Result<CfCreds, (String, u16)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("ManyDesk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| ("md.net".to_string(), 0))?;
    let res = client.post(RELAY).send().await.map_err(|_| ("md.net".to_string(), 0))?;
    let status = res.status().as_u16();
    if !res.status().is_success() {
        return Err((if status == 429 { "md.busy" } else { "md.http" }.into(), status));
    }
    let data: Value = res.json().await.unwrap_or(Value::Null);
    ice_creds(&data, "manydesk", "md.noTurn")
}

/// The TURN entry of a generate-ice-servers answer, valid 24 h.
fn ice_creds(data: &Value, source: &str, none: &str) -> Result<CfCreds, (String, u16)> {
    let turn = data["iceServers"].as_array().and_then(|a| a.iter().find(|s| s["username"].is_string() && s["credential"].is_string()).cloned()).ok_or((none.to_string(), 0))?;
    let urls: Vec<String> = match &turn["urls"] {
        Value::Array(a) => a.iter().filter_map(|u| u.as_str()).filter(|u| u.starts_with("turn")).map(String::from).collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    };
    Ok(CfCreds {
        source: source.into(),
        urls,
        username: turn["username"].as_str().unwrap_or("").into(),
        credential: turn["credential"].as_str().unwrap_or("").into(),
        expires: crate::rv::now_ms() + 86_400_000,
    })
}

/// The server for a new session (None when TURN is off). New Cloudflare credentials
/// are reported on the bus ("turn:cache") so the app can save them.
pub async fn server(cfg: TurnCfg) -> Result<Option<Server>, String> {
    if !cfg.enabled {
        return Ok(None);
    }
    if cfg.provider == "manual" {
        let url = udp_url(&cfg.urls).ok_or_else(|| crate::i18n::t("turn.needUrl"))?;
        let more = udp_urls(&cfg.urls).into_iter().skip(1).collect();
        return Ok(Some(Server { url, username: cfg.username, credential: cfg.credential, more }));
    }
    let own = cfg.provider == "cloudflare";
    let source = if own { cfg.cf_key_id.as_str() } else { "manydesk" };
    // a cache from the other provider (or an older one without a source) is not used
    let creds = match cfg.cf_cache.filter(|c| c.source == source && c.expires > crate::rv::now_ms() + 3_600_000) {
        Some(c) => c,
        None => {
            let fresh = if own { cloudflare(&cfg.cf_key_id, &cfg.cf_token).await } else { manydesk().await };
            let c = fresh.map_err(|(k, st)| crate::i18n::tf(&k, &[("status", &st.to_string())]))?;
            crate::bus::emit("turn:cache", serde_json::to_value(&c).unwrap_or_default());
            c
        }
    };
    let mut urls = udp_urls(&creds.urls).into_iter();
    let url = urls.next().ok_or_else(|| crate::i18n::t("cf.noTurn"))?;
    Ok(Some(Server { url, username: creds.username, credential: creds.credential, more: urls.collect() }))
}

/// Settings → TURN → Test: allocates a relay with these settings.
pub async fn test(cfg: TurnCfg) -> Result<String, String> {
    let mut cfg = cfg;
    cfg.enabled = true;
    let server = server(cfg).await?.ok_or("off")?;
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    let t0 = std::time::Instant::now();
    let (url, relay) = crate::mdp::turn::Relay::allocate_any(&sock, &server).await.map_err(|e| if e.is_empty() { crate::i18n::t("turn.noResp") } else { e })?;
    Ok(format!("{}\n{} → {} ({} ms)", crate::i18n::t("turn.works"), url, relay.relayed, t0.elapsed().as_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_creds_takes_the_turn_entry() {
        let data = json!({ "iceServers": [
            { "urls": ["stun:stun.cloudflare.com:3478"] },
            { "urls": ["turn:turn.cloudflare.com:3478?transport=udp", "turns:turn.cloudflare.com:5349?transport=tcp"], "username": "u", "credential": "c" },
        ]});
        let c = ice_creds(&data, "manydesk", "md.noTurn").unwrap();
        assert_eq!((c.source.as_str(), c.username.as_str(), c.credential.as_str()), ("manydesk", "u", "c"));
        assert_eq!(udp_url(&c.urls).as_deref(), Some("turn:turn.cloudflare.com:3478?transport=udp"));
        let all = ["turn:a:3478?transport=udp", "turn:a:3478?transport=tcp", "turns:a:5349?transport=tcp", "turn:a:443?transport=udp", "turn:a:80?transport=tcp"].map(String::from);
        assert_eq!(udp_urls(&all), ["turn:a:3478?transport=udp", "turn:a:443?transport=udp"]);
        assert!(matches!(ice_creds(&json!({ "iceServers": [] }), "manydesk", "md.noTurn"), Err((k, _)) if k == "md.noTurn"));
    }

    /// Network: the ManyDesk relay's credentials allocate a Cloudflare TURN relay.
    #[test]
    #[ignore = "network"]
    fn relay_allocates() {
        let cfg = crate::store::Store::default().turn;
        let out = tokio::runtime::Runtime::new().unwrap().block_on(test(cfg)).expect("relay");
        assert!(out.contains("turn.cloudflare.com"), "{out}");
    }

    #[test]
    fn older_cache_without_source_still_loads() {
        let c: CfCreds = serde_json::from_value(json!({ "urls": [], "username": "u", "credential": "c", "expires": 1 })).unwrap();
        assert_eq!(c.source, "");
    }
}
