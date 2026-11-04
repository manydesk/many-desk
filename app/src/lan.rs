// The local network: ManyDesk finds ManyDesk on the same LAN and reaches it with no
// internet at all (AnyDesk's Discovery, RustDesk's LAN discovery and direct IP access).
//
// One UDP port, JSON datagrams:
//   {"k":"who","q"}                               who is there? (broadcast, or to one address: direct IP)
//   {"k":"hi","id","name","os","macs","q"}        the answer, with the asker's q (and once at start)
//   {"k":"ev","ev":{…}}                           a rendezvous event, signed as on the relays
// Every rendezvous publish goes here too (broadcast, and to each address a "hi" came from),
// so the handshake works on a LAN that has no internet; the session itself then finds the
// local address as always (mdp/net.rs). The events are checked like the relays' (hash,
// signature, age), so a LAN neighbour can't forge one either.
//
// Wake-on-LAN: the "hi" tells this computer's network cards; a sleeping one is woken with
// the magic packet (6 × FF, then its MAC 16 times) to UDP port 9, broadcast.
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::bus;
use crate::rendezvous::Rendezvous;

pub const PORT: u16 = 21140;
/// A device that said nothing for this long is gone from the list.
const FRESH: Duration = Duration::from_secs(45);

#[derive(Clone)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ip: IpAddr,
    pub seen: Instant,
}

struct Me {
    id: String,
    name: String,
    os: String,
    visible: bool,
}

struct Lan {
    sock: UdpSocket,
    rv: Arc<Rendezvous>,
    me: Mutex<Me>,
    peers: Mutex<HashMap<String, Peer>>,
    /// addresses typed in as direct IP (they may be past a router: no broadcast reaches them)
    direct: Mutex<HashSet<IpAddr>>,
}

static LAN: OnceLock<Arc<Lan>> = OnceLock::new();

fn socket() -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // more than one ManyDesk on a computer (another user, tests) all hear the broadcasts
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    Ok(s.into())
}

/// Starts listening (once). `visible`: answer "who" (Settings → Visible on the local network).
pub fn start(rv: Arc<Rendezvous>, id: &str, name: &str, os: &str, visible: bool) {
    let sock = match socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[lan] port {PORT}: {e}");
            return;
        }
    };
    let me = Me { id: id.into(), name: name.into(), os: os.into(), visible };
    let lan = Arc::new(Lan { sock, rv, me: Mutex::new(me), peers: Mutex::new(HashMap::new()), direct: Mutex::new(HashSet::new()) });
    if LAN.set(lan.clone()).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("lan".into())
        .spawn(move || {
            lan.hello(None, "");
            lan.broadcast(&json!({ "k": "who" }));
            let mut buf = vec![0u8; 65536];
            loop {
                let Ok((n, from)) = lan.sock.recv_from(&mut buf) else {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                };
                if let Ok(v) = serde_json::from_slice::<Value>(&buf[..n]) {
                    lan.on_packet(v, from);
                }
            }
        })
        .ok();
}

impl Lan {
    fn send(&self, msg: &Value, to: SocketAddr) {
        let _ = self.sock.send_to(msg.to_string().as_bytes(), to);
    }

    /// To everyone on every network this computer is on (and the direct IPs).
    fn broadcast(&self, msg: &Value) {
        let data = msg.to_string();
        let mut to: HashSet<IpAddr> = broadcast_addrs().into_iter().map(IpAddr::V4).collect();
        to.extend(self.direct.lock().unwrap().iter().copied());
        for ip in to {
            let _ = self.sock.send_to(data.as_bytes(), SocketAddr::new(ip, PORT));
        }
    }

    fn hello(&self, to: Option<SocketAddr>, q: &str) {
        let msg = {
            let me = self.me.lock().unwrap();
            if !me.visible || me.id.is_empty() {
                return;
            }
            json!({ "k": "hi", "id": me.id, "name": me.name, "os": me.os, "macs": macs(), "q": q })
        };
        match to {
            Some(a) => self.send(&msg, a),
            None => self.broadcast(&msg),
        }
    }

    fn on_packet(&self, v: Value, from: SocketAddr) {
        match v["k"].as_str() {
            // to the asker and to everyone: a second ManyDesk on that computer shares the
            // port, and only one of them gets what is sent to the address
            Some("who") => {
                let q: String = v["q"].as_str().unwrap_or("").chars().take(32).collect();
                self.hello(Some(from), &q);
                self.hello(None, &q);
            }
            Some("hi") => {
                let id = v["id"].as_str().unwrap_or("").to_string();
                if id.len() != 9 || !id.chars().all(|c| c.is_ascii_digit()) || id == self.me.lock().unwrap().id {
                    return;
                }
                let s = |k: &str, n: usize| v[k].as_str().unwrap_or("").chars().take(n).collect::<String>();
                let macs: Vec<String> = v["macs"].as_array().map(|a| a.iter().filter_map(|m| m.as_str()).filter(|m| parse_mac(m).is_some()).take(8).map(String::from).collect()).unwrap_or_default();
                let peer = Peer { id: id.clone(), name: s("name", 60), os: s("os", 20), ip: from.ip(), seen: Instant::now() };
                if self.peers.lock().unwrap().insert(id.clone(), peer).is_none() {
                    eprintln!("[lan] {id} at {}", from.ip());
                }
                bus::emit("lan:peer", json!({ "id": id, "name": s("name", 60), "os": s("os", 20), "macs": macs, "ip": from.ip().to_string(), "q": s("q", 32) }));
            }
            Some("ev") => self.rv.on_event(&v["ev"]),
            _ => {}
        }
    }
}

/// Our name or visibility changed.
pub fn set_me(name: &str, visible: bool) {
    if let Some(l) = LAN.get() {
        let mut me = l.me.lock().unwrap();
        me.name = name.into();
        me.visible = visible;
    }
}

/// Asks the LAN who is there (answers come as "lan:peer").
pub fn scan() {
    if let Some(l) = LAN.get() {
        l.broadcast(&json!({ "k": "who" }));
    }
}

/// Direct IP: asks that one address (and keeps sending it rendezvous events); the answer
/// comes as "lan:peer" with this `q`.
pub fn query(ip: IpAddr, q: &str) {
    if let Some(l) = LAN.get() {
        l.direct.lock().unwrap().insert(ip);
        l.send(&json!({ "k": "who", "q": q }), SocketAddr::new(ip, PORT));
    }
}

/// The devices heard lately, nearest first by name.
pub fn peers() -> Vec<Peer> {
    let Some(l) = LAN.get() else { return Vec::new() };
    let mut out: Vec<Peer> = l.peers.lock().unwrap().values().filter(|p| p.seen.elapsed() < FRESH).cloned().collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

/// Whether `id` answered on the LAN lately (its handshake can go here alone).
pub fn knows(id: &str) -> bool {
    LAN.get().is_some_and(|l| l.peers.lock().unwrap().get(id).is_some_and(|p| p.seen.elapsed() < FRESH))
}

/// A rendezvous event: to the LAN too (called by Rendezvous::publish).
pub fn send_event(ev: &Value) {
    if let Some(l) = LAN.get() {
        let msg = json!({ "k": "ev", "ev": ev });
        let data = msg.to_string();
        if data.len() > 60_000 {
            return;
        }
        let mut to: HashSet<IpAddr> = broadcast_addrs().into_iter().map(IpAddr::V4).collect();
        to.extend(l.direct.lock().unwrap().iter().copied());
        to.extend(l.peers.lock().unwrap().values().filter(|p| p.seen.elapsed() < FRESH).map(|p| p.ip));
        for ip in to {
            let _ = l.sock.send_to(data.as_bytes(), SocketAddr::new(ip, PORT));
        }
    }
}

/// Wake-on-LAN: the magic packet for each of these MACs, to every network here.
pub fn wake(macs: &[String], last_ip: Option<IpAddr>) -> bool {
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return false };
    let _ = sock.set_broadcast(true);
    let mut sent = false;
    let mut to: Vec<IpAddr> = broadcast_addrs().into_iter().map(IpAddr::V4).collect();
    // a directed broadcast to the network it was last seen on (when routers pass it)
    if let Some(IpAddr::V4(ip)) = last_ip {
        let o = ip.octets();
        to.push(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 255)));
    }
    for mac in macs.iter().filter_map(|m| parse_mac(m)) {
        let mut pkt = vec![0xffu8; 6];
        for _ in 0..16 {
            pkt.extend_from_slice(&mac);
        }
        for ip in &to {
            for port in [9, 7] {
                sent |= sock.send_to(&pkt, SocketAddr::new(*ip, port)).is_ok();
            }
        }
    }
    sent
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = s.split([':', '-']).filter_map(|p| u8::from_str_radix(p, 16).ok()).collect();
    let mac: [u8; 6] = parts.try_into().ok()?;
    // all zero, or a made-up one (phones hide theirs: 02:00:00:00:00:00)
    (mac != [0; 6] && mac != [2, 0, 0, 0, 0, 0]).then_some(mac)
}

/// The broadcast address of every IPv4 network here, and 255.255.255.255.
fn broadcast_addrs() -> Vec<Ipv4Addr> {
    let mut out = vec![Ipv4Addr::BROADCAST];
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(v4) = i.addr {
            if v4.ip.is_loopback() {
                continue;
            }
            let b = v4.broadcast.unwrap_or_else(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask)));
            if !out.contains(&b) {
                out.push(b);
            }
        }
    }
    out
}

/// This computer's wired and Wi-Fi network cards (for Wake-on-LAN).
#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn macs() -> Vec<String> {
    static MACS: OnceLock<Vec<String>> = OnceLock::new();
    MACS.get_or_init(|| {
        let mut out: Vec<String> = mac_address::MacAddressIterator::new().map(|it| it.map(|m| m.to_string().to_lowercase()).filter(|m| parse_mac(m).is_some()).collect()).unwrap_or_default();
        // (macOS shows them only with the Local Network permission: 02:00:00:00:00:00 before)
        out.dedup();
        out.truncate(8);
        out
    })
    .clone()
}

/// Phones can't be woken over the network (and hide their MAC).
#[cfg(any(target_os = "ios", target_os = "android"))]
fn macs() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_parse() {
        assert_eq!(parse_mac("a4:83:e7:01:02:0f"), Some([0xa4, 0x83, 0xe7, 1, 2, 0x0f]));
        assert_eq!(parse_mac("A4-83-E7-01-02-0F"), Some([0xa4, 0x83, 0xe7, 1, 2, 0x0f]));
        assert_eq!(parse_mac("02:00:00:00:00:00"), None);
        assert_eq!(parse_mac("nonsense"), None);
    }

    /// MANYDESK_TESTWAKE=1: sends a real magic packet for aa:bb:cc:dd:ee:ff (check with a
    /// listener on UDP 9) and prints this computer's MACs.
    #[test]
    fn wake_sends() {
        if std::env::var_os("MANYDESK_TESTWAKE").is_none() {
            return;
        }
        println!("macs {:?}", macs());
        assert!(wake(&["aa:bb:cc:dd:ee:ff".into()], None));
    }
}
