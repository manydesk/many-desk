// MDP addressing: local + STUN-mapped candidates, then UDP hole punching on the same
// socket that QUIC takes over afterwards.
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

const MAGIC: [u8; 4] = *b"MDP1";
const PUNCH: u8 = 1;
const ACK: u8 = 2;

/// Host candidates for every non-loopback IPv4 interface, plus what STUN reports.
pub async fn gather(sock: &UdpSocket, stun: &[String]) -> Vec<SocketAddr> {
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let mut out: Vec<SocketAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) if !v4.is_link_local() => Some(SocketAddr::new(IpAddr::V4(v4), port)),
            _ => None,
        })
        .collect();
    for a in stun_mapped(sock, stun).await {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

async fn resolve(url: &str) -> Option<SocketAddr> {
    let host = url.trim_start_matches("stun:").trim_start_matches("stuns:");
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:3478") };
    tokio::net::lookup_host(host).await.ok()?.find(|a| a.is_ipv4())
}

/// Binding requests to several STUN servers at once; the distinct mapped addresses.
async fn stun_mapped(sock: &UdpSocket, servers: &[String]) -> Vec<SocketAddr> {
    let mut targets = vec![];
    for url in servers.iter().take(8) {
        if let Ok(Some(a)) = tokio::time::timeout(Duration::from_millis(800), resolve(url)).await {
            targets.push(a);
        }
    }
    let mut txids = HashSet::new();
    for t in &targets {
        let id: [u8; 12] = rand::random();
        let mut req = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
        req.extend_from_slice(&id);
        if sock.send_to(&req, t).await.is_ok() {
            txids.insert(id);
        }
    }
    let mut found = vec![];
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut buf = [0u8; 1500];
    while !txids.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(Ok((n, _))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else { break };
        let b = &buf[..n];
        if n < 20 || b[0..2] != [0x01, 0x01] {
            continue;
        }
        let id: [u8; 12] = b[8..20].try_into().unwrap();
        if !txids.remove(&id) {
            continue;
        }
        if let Some(a) = parse_mapped(b) {
            if !found.contains(&a) {
                found.push(a);
            }
        }
        // One answer is enough once a second server agrees or the rest are slow.
        if found.len() >= 2 {
            break;
        }
    }
    found
}

fn parse_mapped(b: &[u8]) -> Option<SocketAddr> {
    let mut i = 20;
    while i + 4 <= b.len() {
        let typ = u16::from_be_bytes([b[i], b[i + 1]]);
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        let v = b.get(i + 4..i + 4 + len)?;
        if (typ == 0x0020 || typ == 0x0001) && len >= 8 && v[1] == 0x01 {
            let xor = typ == 0x0020;
            let port = u16::from_be_bytes([v[2], v[3]]) ^ if xor { 0x2112 } else { 0 };
            let mut ip = [v[4], v[5], v[6], v[7]];
            if xor {
                for (k, m) in ip.iter_mut().zip([0x21, 0x12, 0xa4, 0x42]) {
                    *k ^= m;
                }
            }
            return Some(SocketAddr::new(IpAddr::from(ip), port));
        }
        i += 4 + ((len + 3) & !3);
    }
    None
}

fn packet(kind: u8, token: &[u8; 16]) -> Vec<u8> {
    let mut p = MAGIC.to_vec();
    p.push(kind);
    p.extend_from_slice(token);
    p
}

/// Both sides send punches to every candidate of the other; whoever hears one answers
/// with an ack to the address it came from (which also covers peer-reflexive addresses
/// behind symmetric NATs). Returns the first address proven to work.
/// The host keeps answering a little longer so the viewer is sure to hear back.
pub async fn punch(sock: &UdpSocket, peers: &[SocketAddr], token: &[u8; 16], host: bool, timeout: Duration) -> Option<SocketAddr> {
    let deadline = Instant::now() + timeout;
    let punch = packet(PUNCH, token);
    let ack = packet(ACK, token);
    let mut found: Option<(SocketAddr, Instant)> = None;
    let mut next_send = Instant::now();
    let mut buf = [0u8; 1500];
    let linger = if host { Duration::from_millis(600) } else { Duration::from_millis(120) };
    loop {
        let now = Instant::now();
        if let Some((a, at)) = found {
            if now.duration_since(at) >= linger {
                return Some(a);
            }
        } else if now >= deadline {
            return None;
        }
        if now >= next_send {
            for p in peers {
                let _ = sock.send_to(&punch, p).await;
            }
            if let Some((a, _)) = found {
                let _ = sock.send_to(&punch, a).await;
            }
            next_send = now + Duration::from_millis(80);
        }
        let wait = next_send.saturating_duration_since(Instant::now()).max(Duration::from_millis(5));
        let Ok(Ok((n, from))) = tokio::time::timeout(wait, sock.recv_from(&mut buf)).await else { continue };
        let b = &buf[..n];
        if n != 21 || b[0..4] != MAGIC || &b[5..21] != token {
            continue;
        }
        if b[4] == PUNCH {
            let _ = sock.send_to(&ack, from).await;
        }
        // Prefer a direct LAN path if one shows up while lingering.
        let better = match found {
            None => true,
            Some((a, _)) => !is_private(a.ip()) && is_private(from.ip()),
        };
        if better {
            found = Some((from, found.map(|f| f.1).unwrap_or(Instant::now())));
        }
    }
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_link_local() || v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00,
    }
}
