// Which TURN server a new session uses: the manual entry, or short-lived credentials
// for the user's own Cloudflare Realtime TURN key (fetched here, cached in settings).
use std::time::Duration;

use serde_json::{json, Value};

use crate::mdp::turn::Server;
use crate::store::{CfCreds, TurnCfg};

/// The first plain-UDP `turn:` address of a list.
fn udp_url(urls: &[String]) -> Option<String> {
    urls.iter().map(|u| u.trim()).find(|u| u.starts_with("turn:") && !u.contains("transport=tcp")).map(String::from)
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
    let turn = data["iceServers"].as_array().and_then(|a| a.iter().find(|s| s["username"].is_string() && s["credential"].is_string()).cloned()).ok_or(("cf.noTurn".to_string(), 0))?;
    let urls: Vec<String> = match &turn["urls"] {
        Value::Array(a) => a.iter().filter_map(|u| u.as_str()).filter(|u| u.starts_with("turn")).map(String::from).collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    };
    Ok(CfCreds {
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
    if cfg.provider != "cloudflare" {
        let url = udp_url(&cfg.urls).ok_or_else(|| crate::i18n::t("turn.needUrl"))?;
        return Ok(Some(Server { url, username: cfg.username, credential: cfg.credential }));
    }
    let creds = match cfg.cf_cache.filter(|c| c.expires > crate::rv::now_ms() + 3_600_000) {
        Some(c) => c,
        None => {
            let c = cloudflare(&cfg.cf_key_id, &cfg.cf_token).await.map_err(|(k, st)| crate::i18n::tf(&k, &[("status", &st.to_string())]))?;
            crate::bus::emit("turn:cache", serde_json::to_value(&c).unwrap_or_default());
            c
        }
    };
    let url = udp_url(&creds.urls).ok_or_else(|| crate::i18n::t("cf.noTurn"))?;
    Ok(Some(Server { url, username: creds.username, credential: creds.credential }))
}

/// Settings → TURN → Test: allocates a relay with these settings.
pub async fn test(cfg: TurnCfg) -> Result<String, String> {
    let mut cfg = cfg;
    cfg.enabled = true;
    let server = server(cfg).await?.ok_or("off")?;
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    let t0 = std::time::Instant::now();
    let relay = tokio::time::timeout(Duration::from_secs(8), crate::mdp::turn::Relay::allocate(&sock, &server)).await.map_err(|_| crate::i18n::t("turn.noResp"))??;
    Ok(format!("{}\n{} → {} ({} ms)", crate::i18n::t("turn.works"), server.url, relay.relayed, t0.elapsed().as_millis()))
}
