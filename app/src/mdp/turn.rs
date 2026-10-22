// TURN relay for MDP (RFC 5766/8656 over UDP), used only when hole punching finds no
// direct path. The relayed address is one more candidate for the peer; the same UDP
// socket that punches and later carries QUIC talks to the TURN server:
//
//   setup:    Allocate (long-term credentials) → relayed address
//   punching: CreatePermission for the peer's addresses; punches through Send indications,
//             replies arrive as Data indications
//   session:  ChannelBind → ChannelData (4-byte header) both ways; Refresh + ChannelBind
//             again every 4 minutes. RelaySocket gives QUIC a plain UDP view of all this.
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use sha1::Sha1;
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;

const MAGIC: u32 = 0x2112_a442;
const ALLOCATE: u16 = 0x0003;
const REFRESH: u16 = 0x0004;
const SEND_IND: u16 = 0x0016;
const DATA_IND: u16 = 0x0017;
const CREATE_PERMISSION: u16 = 0x0008;
const CHANNEL_BIND: u16 = 0x0009;
const SUCCESS: u16 = 0x0100;
const ERROR: u16 = 0x0110;

const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR_CODE: u16 = 0x0009;
const A_CHANNEL_NUMBER: u16 = 0x000c;
const A_LIFETIME: u16 = 0x000d;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;
const A_MAPPED: u16 = 0x0020;

/// The channel used for the one peer of a session.
const CHANNEL: u16 = 0x4000;

/// A TURN server with static (long-term) credentials, as configured or fetched.
#[derive(Clone, Debug)]
pub struct Server {
    pub url: String,
    pub username: String,
    pub credential: String,
}

// ---- STUN messages ----

struct Msg {
    buf: Vec<u8>,
}

impl Msg {
    fn new(kind: u16, txid: &[u8; 12]) -> Msg {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&kind.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.extend_from_slice(txid);
        Msg { buf }
    }

    fn attr(&mut self, t: u16, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(&t.to_be_bytes());
        self.buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(v);
        while self.buf.len() % 4 != 0 {
            self.buf.push(0);
        }
        let len = (self.buf.len() - 20) as u16;
        self.buf[2..4].copy_from_slice(&len.to_be_bytes());
        self
    }

    fn addr(&mut self, t: u16, a: SocketAddr) -> &mut Self {
        let v = xor_addr(a);
        self.attr(t, &v)
    }

    /// USERNAME, REALM, NONCE and MESSAGE-INTEGRITY (HMAC-SHA1 with MD5(user:realm:pass)).
    fn auth(&mut self, a: &Auth) -> Vec<u8> {
        self.attr(A_USERNAME, a.user.as_bytes()).attr(A_REALM, a.realm.as_bytes()).attr(A_NONCE, &a.nonce);
        // The length must already count the integrity attribute (4 + 20 bytes).
        let len = (self.buf.len() - 20 + 24) as u16;
        self.buf[2..4].copy_from_slice(&len.to_be_bytes());
        let mut mac = <Hmac<Sha1>>::new_from_slice(&a.key).expect("hmac key");
        mac.update(&self.buf);
        let sig = mac.finalize().into_bytes();
        self.attr(A_INTEGRITY, &sig);
        self.buf.clone()
    }
}

fn xor_addr(a: SocketAddr) -> Vec<u8> {
    let mut v = vec![0, 1];
    v.extend_from_slice(&(a.port() ^ (MAGIC >> 16) as u16).to_be_bytes());
    match a.ip() {
        IpAddr::V4(ip) => {
            let m = MAGIC.to_be_bytes();
            v.extend(ip.octets().iter().zip(m).map(|(b, k)| b ^ k));
        }
        IpAddr::V6(_) => {} // IPv4 only (candidates are IPv4)
    }
    v
}

fn parse_xor_addr(v: &[u8]) -> Option<SocketAddr> {
    if v.len() < 8 || v[1] != 1 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC >> 16) as u16;
    let m = MAGIC.to_be_bytes();
    let ip = [v[4] ^ m[0], v[5] ^ m[1], v[6] ^ m[2], v[7] ^ m[3]];
    Some(SocketAddr::new(IpAddr::from(ip), port))
}

struct Parsed<'a> {
    kind: u16,
    txid: [u8; 12],
    attrs: Vec<(u16, &'a [u8])>,
}

impl<'a> Parsed<'a> {
    fn get(&self, t: u16) -> Option<&'a [u8]> {
        self.attrs.iter().find(|a| a.0 == t).map(|a| a.1)
    }
    fn error(&self) -> Option<u16> {
        self.get(A_ERROR_CODE).filter(|v| v.len() >= 4).map(|v| v[2] as u16 * 100 + v[3] as u16)
    }
}

fn parse(b: &[u8]) -> Option<Parsed<'_>> {
    if b.len() < 20 || b[0] & 0xc0 != 0 || u32::from_be_bytes(b[4..8].try_into().ok()?) != MAGIC {
        return None;
    }
    let len = u16::from_be_bytes([b[2], b[3]]) as usize;
    let body = b.get(20..20 + len)?;
    let mut attrs = vec![];
    let mut i = 0;
    while i + 4 <= body.len() {
        let t = u16::from_be_bytes([body[i], body[i + 1]]);
        let l = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let v = body.get(i + 4..i + 4 + l)?;
        attrs.push((t, v));
        i += 4 + ((l + 3) & !3);
    }
    Some(Parsed { kind: u16::from_be_bytes([b[0], b[1]]), txid: b[8..20].try_into().ok()?, attrs })
}

/// A packet from the TURN server that carries peer data: (peer, payload range).
fn unwrap(b: &[u8], channel_peer: Option<SocketAddr>) -> Option<(SocketAddr, std::ops::Range<usize>)> {
    if b.len() >= 4 && (0x40..0x80).contains(&b[0]) {
        let len = u16::from_be_bytes([b[2], b[3]]) as usize;
        return channel_peer.filter(|_| 4 + len <= b.len()).map(|p| (p, 4..4 + len));
    }
    let p = parse(b)?;
    if p.kind != DATA_IND {
        return None;
    }
    let peer = parse_xor_addr(p.get(A_PEER)?)?;
    let data = p.get(A_DATA)?;
    let start = data.as_ptr() as usize - b.as_ptr() as usize;
    Some((peer, start..start + data.len()))
}

#[derive(Clone)]
struct Auth {
    user: String,
    realm: String,
    nonce: Vec<u8>,
    key: Vec<u8>,
}

// ---- the allocation ----

pub struct Relay {
    pub server: SocketAddr,
    /// our address on the TURN server (a candidate for the peer)
    pub relayed: SocketAddr,
    auth: Mutex<Auth>,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Relay({} via {})", self.relayed, self.server)
    }
}

async fn resolve(url: &str) -> Result<SocketAddr, String> {
    // turn:host[:port][?transport=udp] — TCP/TLS transports are not supported here.
    let (scheme, rest) = url.split_once(':').ok_or("bad TURN url")?;
    if scheme != "turn" {
        return Err("only turn: (UDP) is supported".into());
    }
    let (host, query) = rest.split_once('?').unwrap_or((rest, ""));
    if query.contains("transport=tcp") {
        return Err("only UDP transport is supported".into());
    }
    let host = if host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { host.to_string() } else { format!("{host}:3478") };
    tokio::net::lookup_host(host).await.map_err(|e| e.to_string())?.find(|a| a.is_ipv4()).ok_or_else(|| "no IPv4 address".into())
}

/// Sends `req` (built by `make` with the current auth) and waits for its answer,
/// retrying once with fresh realm/nonce on 401/438. Other packets are ignored.
async fn transact<'b>(
    sock: &UdpSocket,
    server: SocketAddr,
    auth: &mut Option<Auth>,
    creds: (&str, &str),
    make: impl Fn(&[u8; 12], Option<&Auth>) -> Vec<u8>,
    buf: &'b mut [u8],
) -> Result<usize, String> {
    for _ in 0..3 {
        let txid: [u8; 12] = rand::random();
        let req = make(&txid, auth.as_ref());
        let mut answer = None;
        'retry: for wait in [500u64, 1000, 1500] {
            sock.send_to(&req, server).await.map_err(|e| e.to_string())?;
            let until = tokio::time::Instant::now() + Duration::from_millis(wait);
            loop {
                let Ok(Ok((n, from))) = tokio::time::timeout_at(until, sock.recv_from(buf)).await else { continue 'retry };
                if from != server {
                    continue;
                }
                if parse(&buf[..n]).is_some_and(|p| p.txid == txid) {
                    answer = Some(n);
                    break 'retry;
                }
            }
        }
        let n = answer.ok_or("TURN server did not answer")?;
        let p = parse(&buf[..n]).unwrap();
        if p.kind & 0x0110 == SUCCESS {
            return Ok(n);
        }
        match p.error() {
            Some(401) | Some(438) => {
                let realm = p.get(A_REALM).map(|r| String::from_utf8_lossy(r).to_string()).or_else(|| auth.as_ref().map(|a| a.realm.clone())).unwrap_or_default();
                let nonce = p.get(A_NONCE).ok_or("TURN: no nonce")?.to_vec();
                if p.error() == Some(401) && auth.as_ref().is_some_and(|a| a.nonce == nonce && a.realm == realm) {
                    return Err("TURN: wrong username or password".into());
                }
                let key = Md5::digest(format!("{}:{}:{}", creds.0, realm, creds.1).as_bytes()).to_vec();
                *auth = Some(Auth { user: creds.0.to_string(), realm, nonce, key });
            }
            Some(code) => return Err(format!("TURN error {code}")),
            None => return Err("TURN: bad answer".into()),
        }
    }
    Err("TURN: authentication failed".into())
}

impl Relay {
    /// Allocates a relayed address on `server` from this socket.
    pub async fn allocate(sock: &UdpSocket, server: &Server) -> Result<Relay, String> {
        let addr = resolve(&server.url).await?;
        let mut auth = None;
        let mut buf = vec![0u8; 1500];
        let creds = (server.username.as_str(), server.credential.as_str());
        let n = transact(
            sock,
            addr,
            &mut auth,
            creds,
            |txid, a| {
                let mut m = Msg::new(ALLOCATE, txid);
                m.attr(A_TRANSPORT, &[17, 0, 0, 0]).attr(A_LIFETIME, &600u32.to_be_bytes());
                match a {
                    Some(a) => m.auth(a),
                    None => m.buf.clone(),
                }
            },
            &mut buf,
        )
        .await?;
        let p = parse(&buf[..n]).ok_or("TURN: bad answer")?;
        let relayed = p.get(A_RELAYED).and_then(parse_xor_addr).ok_or("TURN: no relayed address")?;
        let _mapped = p.get(A_MAPPED).and_then(parse_xor_addr);
        Ok(Relay { server: addr, relayed, auth: Mutex::new(auth.ok_or("TURN: no auth")?) })
    }

    async fn request(&self, sock: &UdpSocket, kind: u16, attrs: &[(u16, Vec<u8>)]) -> Result<(), String> {
        let mut auth = Some(self.auth.lock().unwrap().clone());
        let a = auth.as_ref().unwrap();
        let creds = (a.user.clone(), String::new());
        // The key is already derived; transact only needs the password for a new realm,
        // which does not change for the same server, so the derived key is reused.
        let key = a.key.clone();
        let mut buf = vec![0u8; 1500];
        let r = transact(
            sock,
            self.server,
            &mut auth,
            (&creds.0, &creds.1),
            |txid, a| {
                let mut m = Msg::new(kind, txid);
                for (t, v) in attrs {
                    m.attr(*t, v);
                }
                let mut a = a.cloned().unwrap();
                a.key = key.clone();
                m.auth(&a)
            },
            &mut buf,
        )
        .await;
        if let Some(mut a) = auth {
            a.key = key;
            *self.auth.lock().unwrap() = a;
        }
        r.map(|_| ())
    }

    /// Lets these peers' packets through the relay (IP only; ports do not matter).
    /// Public addresses first; private ones separately, since servers may refuse those
    /// and one refused address fails the whole request.
    pub async fn permit(&self, sock: &UdpSocket, peers: &[SocketAddr]) -> Result<(), String> {
        let mut public: Vec<(u16, Vec<u8>)> = vec![];
        let mut private: Vec<(u16, Vec<u8>)> = vec![];
        let mut seen: Vec<IpAddr> = vec![];
        for p in peers.iter().filter(|p| p.is_ipv4()) {
            if seen.contains(&p.ip()) {
                continue;
            }
            seen.push(p.ip());
            let list = if super::net::is_private(p.ip()) { &mut private } else { &mut public };
            list.push((A_PEER, xor_addr(*p)));
        }
        if !private.is_empty() {
            let _ = self.request(sock, CREATE_PERMISSION, &private).await;
        }
        if public.is_empty() {
            return Ok(());
        }
        self.request(sock, CREATE_PERMISSION, &public).await
    }

    /// Binds the session's channel to the chosen peer (also renews its permission).
    pub async fn bind(&self, sock: &UdpSocket, peer: SocketAddr) -> Result<(), String> {
        self.request(sock, CHANNEL_BIND, &[(A_CHANNEL_NUMBER, vec![(CHANNEL >> 8) as u8, CHANNEL as u8, 0, 0]), (A_PEER, xor_addr(peer))]).await
    }

    /// A Send indication: `data` for `peer` through the relay.
    pub fn wrap(&self, peer: SocketAddr, data: &[u8]) -> Vec<u8> {
        let mut m = Msg::new(SEND_IND, &rand::random());
        m.addr(A_PEER, peer).attr(A_DATA, data);
        m.buf
    }

    /// Peer data inside a packet from the TURN server (Data indication).
    pub fn unwrap(b: &[u8]) -> Option<(SocketAddr, std::ops::Range<usize>)> {
        unwrap(b, None)
    }

    /// Refresh/ChannelBind requests built with the current nonce (answers are read by
    /// RelaySocket; a stale nonce is picked up from the 438 answer).
    fn keepalive(&self, peer: SocketAddr) -> Vec<Vec<u8>> {
        let a = self.auth.lock().unwrap().clone();
        let mut r = Msg::new(REFRESH, &rand::random());
        r.attr(A_LIFETIME, &600u32.to_be_bytes());
        let mut b = Msg::new(CHANNEL_BIND, &rand::random());
        b.attr(A_CHANNEL_NUMBER, &[(CHANNEL >> 8) as u8, CHANNEL as u8, 0, 0]).addr(A_PEER, peer);
        vec![r.auth(&a), b.auth(&a)]
    }

    fn release(&self) -> Vec<u8> {
        let a = self.auth.lock().unwrap().clone();
        let mut r = Msg::new(REFRESH, &rand::random());
        r.attr(A_LIFETIME, &0u32.to_be_bytes());
        r.auth(&a)
    }

    /// An answer from the server: remember a new nonce (438) so the next keepalive works.
    fn on_answer(&self, b: &[u8]) -> bool {
        let Some(p) = parse(b) else { return false };
        if p.kind & 0x0110 == ERROR && p.error() == Some(438) {
            if let Some(n) = p.get(A_NONCE) {
                self.auth.lock().unwrap().nonce = n.to_vec();
                return true;
            }
        }
        false
    }
}

// ---- QUIC over the relay ----

/// QUIC's socket when the path goes through our TURN allocation: datagrams to the peer
/// become ChannelData to the server, ChannelData from the server comes out as datagrams
/// from the peer. Anything else is passed through unchanged.
pub struct RelaySocket {
    io: Arc<UdpSocket>,
    relay: Arc<Relay>,
    peer: SocketAddr,
}

impl fmt::Debug for RelaySocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelaySocket({:?} → {})", self.relay, self.peer)
    }
}

impl RelaySocket {
    pub fn new(io: UdpSocket, relay: Relay, peer: SocketAddr) -> Arc<RelaySocket> {
        let s = Arc::new(RelaySocket { io: Arc::new(io), relay: Arc::new(relay), peer });
        // Allocation, permission and channel last 10 / 5 / 10 minutes: renew every 4.
        let weak = Arc::downgrade(&s);
        crate::rt::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(240)).await;
                let Some(s) = weak.upgrade() else { break };
                for m in s.relay.keepalive(s.peer) {
                    let _ = s.io.send_to(&m, s.relay.server).await;
                }
            }
        });
        s
    }

    fn send_one(&self, dest: SocketAddr, data: &[u8]) -> io::Result<()> {
        if dest != self.peer {
            return self.io.try_send_to(data, dest).map(|_| ());
        }
        let mut b = Vec::with_capacity(data.len() + 4);
        b.extend_from_slice(&CHANNEL.to_be_bytes());
        b.extend_from_slice(&(data.len() as u16).to_be_bytes());
        b.extend_from_slice(data);
        self.io.try_send_to(&b, self.relay.server).map(|_| ())
    }
}

impl Drop for RelaySocket {
    fn drop(&mut self) {
        // Free the allocation right away instead of letting it time out.
        let _ = self.io.try_send_to(&self.relay.release(), self.relay.server);
    }
}

#[derive(Debug)]
struct Poller(Arc<RelaySocket>);

impl UdpPoller for Poller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.0.io.poll_send_ready(cx)
    }
}

impl AsyncUdpSocket for RelaySocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Poller(self))
    }

    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        match t.segment_size {
            Some(seg) if seg < t.contents.len() => {
                for chunk in t.contents.chunks(seg) {
                    self.send_one(t.destination, chunk)?;
                }
                Ok(())
            }
            _ => self.send_one(t.destination, t.contents),
        }
    }

    fn poll_recv(&self, cx: &mut Context, bufs: &mut [IoSliceMut<'_>], meta: &mut [RecvMeta]) -> Poll<io::Result<usize>> {
        loop {
            let mut rb = ReadBuf::new(&mut bufs[0]);
            let from = match self.io.poll_recv_from(cx, &mut rb) {
                Poll::Ready(Ok(a)) => a,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };
            let n = rb.filled().len();
            let (addr, len) = if from == self.relay.server {
                match unwrap(&bufs[0][..n], Some(self.peer)) {
                    Some((peer, r)) => {
                        let len = r.len();
                        bufs[0].copy_within(r, 0);
                        (peer, len)
                    }
                    None => {
                        // A keepalive answer; on a stale nonce, renew at once.
                        if self.relay.on_answer(&bufs[0][..n]) {
                            for m in self.relay.keepalive(self.peer) {
                                let _ = self.io.try_send_to(&m, self.relay.server);
                            }
                        }
                        continue;
                    }
                }
            } else {
                (from, n)
            };
            meta[0] = RecvMeta { addr, len, stride: len, ecn: None, dst_ip: None };
            return Poll::Ready(Ok(1));
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MANYDESK_TEST_TURN="turn:host:3478|user|pass" cargo test turn_allocates -- --ignored
    #[test]
    #[ignore]
    fn turn_allocates() {
        let spec = std::env::var("MANYDESK_TEST_TURN").unwrap();
        let p: Vec<&str> = spec.split('|').collect();
        let server = Server { url: p[0].into(), username: p[1].into(), credential: p[2].into() };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let sock = UdpSocket::bind("0.0.0.0:0").await.unwrap();
            let r = Relay::allocate(&sock, &server).await.unwrap();
            println!("relayed {}", r.relayed);
            r.permit(&sock, &["8.8.8.8:1".parse().unwrap(), "10.0.0.1:1".parse().unwrap()]).await.unwrap();
            r.bind(&sock, "8.8.8.8:5000".parse().unwrap()).await.unwrap();
            let bad = Server { credential: "wrong".into(), ..server };
            assert!(Relay::allocate(&sock, &bad).await.is_err());
        });
    }

    #[test]
    fn xor_address_round_trip() {
        let a: SocketAddr = "203.0.113.7:50123".parse().unwrap();
        assert_eq!(parse_xor_addr(&xor_addr(a)), Some(a));
    }

    #[test]
    fn send_indication_unwraps_like_data() {
        let peer: SocketAddr = "198.51.100.2:4000".parse().unwrap();
        let mut m = Msg::new(DATA_IND, &[7; 12]);
        m.addr(A_PEER, peer).attr(A_DATA, b"hello");
        let (p, r) = unwrap(&m.buf, None).unwrap();
        assert_eq!(p, peer);
        assert_eq!(&m.buf[r], b"hello");
        let cd = [0x40, 0x00, 0, 3, b'a', b'b', b'c'];
        let (p, r) = unwrap(&cd, Some(peer)).unwrap();
        assert_eq!((p, &cd[r]), (peer, &b"abc"[..]));
    }

    #[test]
    fn integrity_matches_rfc5769_shape() {
        // MESSAGE-INTEGRITY is the last attribute and the header length counts it.
        let a = Auth { user: "u".into(), realm: "r".into(), nonce: b"n".to_vec(), key: Md5::digest(b"u:r:p").to_vec() };
        let b = Msg::new(ALLOCATE, &[1; 12]).auth(&a);
        let p = parse(&b).unwrap();
        assert_eq!(p.attrs.last().unwrap().0, A_INTEGRITY);
        assert_eq!(u16::from_be_bytes([b[2], b[3]]) as usize, b.len() - 20);
    }
}
