// Settings → Diagnostics: what kind of NAT this computer is behind (one local socket asks
// several STUN servers for its public mapping), which STUN servers answer, and which
// rendezvous channels are open. Same report as the web build.
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::net::UdpSocket;

use crate::i18n::t;

async fn resolve(url: &str) -> Option<SocketAddr> {
    let host = url.trim_start_matches("stun:").trim_start_matches("stuns:");
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:3478") };
    tokio::time::timeout(Duration::from_secs(2), tokio::net::lookup_host(host)).await.ok()?.ok()?.find(|a| a.is_ipv4())
}

fn mapped(b: &[u8]) -> Option<SocketAddr> {
    let mut i = 20;
    while i + 4 <= b.len() {
        let typ = u16::from_be_bytes([b[i], b[i + 1]]);
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        let v = b.get(i + 4..i + 4 + len)?;
        if (typ == 0x0020 || typ == 0x0001) && len >= 8 && v[1] == 1 {
            let x = typ == 0x0020;
            let port = u16::from_be_bytes([v[2], v[3]]) ^ if x { 0x2112 } else { 0 };
            let mut ip = [v[4], v[5], v[6], v[7]];
            if x {
                for (k, m) in ip.iter_mut().zip([0x21, 0x12, 0xa4, 0x42]) {
                    *k ^= m;
                }
            }
            return Some(SocketAddr::from((ip, port)));
        }
        i += 4 + ((len + 3) & !3);
    }
    None
}

/// The plain-text report shown under "Check network".
pub async fn run(stun: Vec<String>, rv: Value) -> (String, bool) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0").await else { return (t("nat.blocked"), false) };
    let local = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let mut targets = vec![];
    for url in &stun {
        if let Some(a) = resolve(url).await {
            targets.push((url.clone(), a));
        }
    }
    let mut sent: HashMap<[u8; 12], (String, Instant)> = HashMap::new();
    for (url, a) in &targets {
        let id: [u8; 12] = rand::random();
        let mut req = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
        req.extend_from_slice(&id);
        if sock.send_to(&req, a).await.is_ok() {
            sent.insert(id, (url.clone(), Instant::now()));
        }
    }
    let mut answers: Vec<(String, u128, SocketAddr)> = vec![];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 1500];
    while !sent.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(Ok((n, _))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else { break };
        if n < 20 || buf[0..2] != [0x01, 0x01] {
            continue;
        }
        let id: [u8; 12] = buf[8..20].try_into().unwrap();
        if let (Some((url, at)), Some(m)) = (sent.remove(&id), mapped(&buf[..n])) {
            answers.push((url, at.elapsed().as_millis(), m));
        }
    }
    let ports: BTreeSet<u16> = answers.iter().map(|a| a.2.port()).collect();
    let ips: BTreeSet<String> = answers.iter().map(|a| a.2.ip().to_string()).collect();
    let hosts: Vec<String> = if_addrs::get_if_addrs().unwrap_or_default().into_iter().filter(|i| !i.is_loopback() && i.ip().is_ipv4()).map(|i| i.ip().to_string()).collect();
    let open = answers.iter().any(|a| hosts.contains(&a.2.ip().to_string()));
    let (kind, verdict, ok) = if answers.is_empty() {
        ("blocked", t("nat.blocked"), false)
    } else if open {
        ("open", t("nat.open"), true)
    } else if ports.len() > 1 {
        ("symmetric", t("nat.symmetric"), false)
    } else if ips.len() > 1 {
        ("multi-ip", t("nat.multi"), true)
    } else {
        ("cone", t("nat.cone"), true)
    };
    let mut lines = vec![
        format!("{} · {}", crate::core::platform(), crate::rv::now_ms() / 1000),
        format!("NAT: {kind}"),
        verdict,
        format!("Public IP: {}", if ips.is_empty() { "—".into() } else { ips.into_iter().collect::<Vec<_>>().join(", ") }),
        format!("Local addresses: {}, port preserved: {}", hosts.len(), if ports.iter().all(|p| *p == local) && !ports.is_empty() { "yes" } else { "no" }),
        "Mappings (local → public, server):".into(),
    ];
    for (url, _, m) in &answers {
        lines.push(format!("  :{local} → {m}  {url}"));
    }
    lines.push("STUN servers:".into());
    for url in &stun {
        match answers.iter().find(|a| &a.0 == url) {
            Some((_, ms, _)) => lines.push(format!("  ✓ {url}  {ms} ms")),
            None => lines.push(format!("  ✗ {url}  —")),
        }
    }
    lines.push(format!("Rendezvous channels: {}/{} open", rv["connected"], rv["total"]));
    let off = rv["clockOffsetS"].as_i64().unwrap_or(0);
    lines.push(format!(
        "System clock: {}",
        if off.abs() < 3 { "ok".to_string() } else if off > 0 { format!("{off} s behind (network time used)") } else { format!("{} s ahead (network time used)", -off) }
    ));
    for r in rv["relays"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  {} {} {}",
            if r["open"].as_bool().unwrap_or(false) { "✓" } else { "✗" },
            if r["kind"] == "mqtt" { "MQTT " } else { "Nostr" },
            r["url"].as_str().unwrap_or("")
        ));
    }
    (lines.join("\n"), ok)
}
