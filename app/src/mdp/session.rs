// MDP — ManyDesk's native protocol between two current-version peers (like AnyDesk's
// own transport): QUIC (TLS 1.3, BBR) over a hole-punched UDP socket.
//
//   viewer = QUIC client, host = QUIC server; certificates pinned both ways (tls.rs)
//   bi stream 'c': control messages        bi stream 'f': file transfer
//   uni stream per video frame (host → viewer): [key u8][ts u32][seq u32] + Annex-B H.264
//   datagram [1] (viewer → host): keyframe request
//   datagram [2] + Opus packet (both ways): voice, 20 ms per packet
//   uni stream per camera frame (both ways): [0x10 | key][ts u32][seq u32] + Annex-B H.264
//   datagram [3]: keyframe request for the camera picture
//
// Each frame gets its own stream, so a lost packet only delays that frame; once a newer
// keyframe exists, older frames still in flight are reset instead of delivered late.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use super::{net, tls, turn};
use crate::media::capture;
use crate::media::encode::{EncodedFrame, Sink, Video};

const CH_CTL: u8 = b'c';
const CH_FILE: u8 = b'f';
/// TCP tunnel: one bi stream per forwarded connection, opened by the viewer:
/// tag, u16 length + JSON {"h": host, "p": port}; the host answers one byte (0 = connected).
const CH_TUNNEL: u8 = b't';
const DGRAM_KEYFRAME: u8 = 1;
const DGRAM_AUDIO: u8 = 2;
const DGRAM_CAM_KEY: u8 = 3;
/// First byte of a uni stream: this frame is the camera picture, not the screen.
const UNI_CAMERA: u8 = 0x10;

#[derive(Deserialize, Clone)]
pub struct Peer {
    pub cands: Vec<String>,
    pub fp: String,
    #[serde(default)]
    pub token: String,
}

/// What a session delivers to the app besides JSON events.
pub enum Media {
    /// Annex-B H.264 access unit (viewer)
    Video { key: bool, ts: u32, data: Vec<u8> },
    /// binary file-transfer chunk
    File(Vec<u8>),
    /// one Opus packet
    Audio(Vec<u8>),
    /// Annex-B H.264 access unit of the other side's camera (in order, gaps skipped)
    Camera { key: bool, data: Vec<u8> },
}

pub type OnEvent = Arc<dyn Fn(Value) + Send + Sync>;
pub type OnMedia = Arc<dyn Fn(Media) + Send + Sync>;

pub struct Session {
    host: bool,
    events: OnEvent,
    media: OnMedia,
    id: tls::Identity,
    token: [u8; 16],
    sock: Mutex<Option<tokio::net::UdpSocket>>,
    endpoint: Mutex<Option<quinn::Endpoint>>,
    conn: Mutex<Option<quinn::Connection>>,
    ctl: AsyncMutex<Option<quinn::SendStream>>,
    file: AsyncMutex<Option<quinn::SendStream>>,
    /// one file (fstart, chunks, fend) on the file stream at a time
    file_turn: AsyncMutex<()>,
    video: Mutex<Option<Arc<Video>>>,
    /// where encoded frames go (kept to restart the encoder on another display)
    sink: Mutex<Option<Arc<dyn Sink>>>,
    /// host: no video until the viewer is let in (unattended access waits for the password)
    hold: AtomicBool,
    sending: AtomicBool,
    peer_fp: Mutex<String>,
    path: Mutex<Option<SocketAddr>>,
    /// our TURN allocation (when configured and reachable)
    relay: Mutex<Option<turn::Relay>>,
    relayed: AtomicBool,
    closed: AtomicBool,
    /// host: the viewer may open TCP tunnels (the Tunnel permission)
    tunnel_ok: AtomicBool,
    /// tunnel connections open now (both sides)
    pub tunnels: AtomicUsize,
    /// host: when the keyframe now on its way started (requests meanwhile are ignored)
    key_in_flight: Mutex<Option<Instant>>,
    frames_in: AtomicU64,
    bytes_in: AtomicU64,
    /// host: the viewer draws the pointer itself (position/shape on the control stream)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    cursor_apart: AtomicBool,
    /// host: the viewer may show its camera here (the Camera permission)
    camera_ok: AtomicBool,
    /// our camera: sequence of the last frame sent, and "the other side wants a keyframe"
    cam_seq: AtomicU32,
    cam_key: Arc<AtomicBool>,
    /// the other side's camera: next sequence number expected, last keyframe request
    cam_rx: Mutex<(Option<u32>, Option<Instant>)>,
}

impl Session {
    fn emit(&self, v: Value) {
        (self.events)(v);
    }

    fn conn(&self) -> Option<quinn::Connection> {
        self.conn.lock().unwrap().clone()
    }

    fn fail(&self, reason: &str) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.emit(json!({ "e": "closed", "reason": reason }));
        }
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---- framing on the bi streams: [kind u8: 0 text, 1 binary][len u32][payload] ----

async fn write_msg(stream: &AsyncMutex<Option<quinn::SendStream>>, kind: u8, data: &[u8]) -> Result<(), String> {
    let mut g = stream.lock().await;
    let s = g.as_mut().ok_or("not connected")?;
    let mut head = [0u8; 5];
    head[0] = kind;
    head[1..5].copy_from_slice(&(data.len() as u32).to_be_bytes());
    s.write_all(&head).await.map_err(err)?;
    s.write_all(data).await.map_err(err)
}

/// Host: a tunnel stream — connect where the viewer asked and pipe both ways.
async fn serve_tunnel(me: Arc<Session>, mut tx: quinn::SendStream, mut rx: quinn::RecvStream) {
    let mut tag = [0u8; 3];
    if rx.read_exact(&mut tag).await.is_err() || tag[0] != CH_TUNNEL {
        return;
    }
    let mut head = vec![0u8; u16::from_be_bytes([tag[1], tag[2]]) as usize];
    if rx.read_exact(&mut head).await.is_err() {
        return;
    }
    if !me.tunnel_ok.load(Ordering::SeqCst) {
        let _ = tx.write_all(&[2]).await;
        let _ = tx.finish();
        return;
    }
    let Ok(v) = serde_json::from_slice::<Value>(&head) else { return };
    let (host, port) = (v["h"].as_str().unwrap_or("127.0.0.1").to_string(), v["p"].as_u64().unwrap_or(0) as u16);
    let tcp = match tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(t)) => t,
        _ => {
            let _ = tx.write_all(&[1]).await;
            let _ = tx.finish();
            return;
        }
    };
    if tx.write_all(&[0]).await.is_err() {
        return;
    }
    pipe(&me, tcp, tx, rx).await;
}

/// Copies between a TCP connection and a tunnel stream until either side closes.
pub async fn pipe(me: &Session, tcp: tokio::net::TcpStream, mut tx: quinn::SendStream, mut rx: quinn::RecvStream) {
    me.tunnels.fetch_add(1, Ordering::SeqCst);
    let _ = tcp.set_nodelay(true);
    let (mut r, mut w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut r, &mut tx).await;
        let _ = tx.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut rx, &mut w).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut w).await;
    };
    tokio::join!(up, down);
    me.tunnels.fetch_sub(1, Ordering::SeqCst);
}

async fn read_msgs(me: Arc<Session>, mut rx: quinn::RecvStream, ch: u8) {
    let mut head = [0u8; 5];
    while rx.read_exact(&mut head).await.is_ok() {
        let len = u32::from_be_bytes(head[1..5].try_into().unwrap()) as usize;
        if len > 64 << 20 {
            break;
        }
        let mut data = vec![0u8; len];
        if rx.read_exact(&mut data).await.is_err() {
            break;
        }
        match (ch, head[0]) {
            (CH_CTL, _) => me.emit(json!({ "e": "ctl", "text": String::from_utf8_lossy(&data) })),
            (_, 0) => me.emit(json!({ "e": "file", "text": String::from_utf8_lossy(&data) })),
            _ => (me.media)(Media::File(data)),
        }
    }
}

// ---- host: encoded frames → one QUIC uni stream each ----

struct QuicSink {
    tx: tokio::sync::mpsc::Sender<EncodedFrame>,
    conn: quinn::Connection,
    /// frames sent but not yet all delivered: sequence → when sending started
    pending: Arc<Mutex<BTreeMap<u32, Instant>>>,
    /// when the encoder last got the go-ahead (see `congested`)
    last_go: Mutex<Instant>,
}

/// However backed up the path looks, a frame goes at least this often: the picture never
/// freezes (a wrong backlog reading, a very slow link) — it just updates slowly.
const MIN_FRAME_GAP: Duration = Duration::from_millis(500);
/// A frame not delivered within this is given up (its stream reset), so one stuck frame
/// can't keep the path "backed up" forever.
const FRAME_GIVE_UP: Duration = Duration::from_secs(3);
/// A delta is given up only after it had time to go out at this rate (bits/s), so a big
/// frame on a slow link is not mistaken for a stuck one. Keyframes are never given up.
const GIVE_UP_RATE: f64 = 200_000.0;
/// How long a keyframe on its way blocks further keyframe requests.
const KEY_WAIT: Duration = Duration::from_secs(10);

/// How long the oldest frame may be on its way beyond one round trip before the path
/// counts as backed up (then frames are skipped at the encoder).
const BACKLOG: Duration = Duration::from_millis(150);
/// Beyond this, frames already encoded are dropped too (and a keyframe resyncs).
const BACKLOG_DROP: Duration = Duration::from_millis(1000);

fn backlog(pending: &Mutex<BTreeMap<u32, Instant>>, conn: &quinn::Connection) -> Duration {
    let oldest = pending.lock().unwrap().values().next().copied();
    oldest.map(|t| t.elapsed().saturating_sub(conn.rtt())).unwrap_or_default()
}

impl Sink for QuicSink {
    fn push(&self, f: EncodedFrame) -> bool {
        self.tx.try_send(f).is_ok()
    }
    fn closed(&self) -> bool {
        self.tx.is_closed()
    }
    fn congested(&self) -> bool {
        let chan = self.tx.capacity() == 0;
        // Jitter (Wi-Fi) delays single frames by about a round trip; only a backlog beyond
        // that and BACKLOG means the path can't keep up.
        let back = backlog(&self.pending, &self.conn) > BACKLOG.max(self.conn.rtt());
        let mut last = self.last_go.lock().unwrap();
        if (chan || back) && last.elapsed() < MIN_FRAME_GAP {
            crate::perf::tick(if chan { "cong-channel" } else { "cong-backlog" });
            return true;
        }
        *last = Instant::now();
        false
    }
}

fn start_video_sender(me: Arc<Session>, conn: quinn::Connection, video: Arc<Video>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EncodedFrame>(2);
    let pending: Arc<Mutex<BTreeMap<u32, Instant>>> = Default::default();
    let sink: Arc<dyn Sink> = Arc::new(QuicSink { tx, conn: conn.clone(), pending: pending.clone(), last_go: Mutex::new(Instant::now()) });
    *me.sink.lock().unwrap() = Some(sink.clone());
    video.start(sink);
    // MANYDESK_PERF: what actually goes out on the wire (QUIC, all streams), per second.
    if std::env::var_os("MANYDESK_PERF").is_some() {
        let (c, m) = (conn.clone(), me.clone());
        crate::rt::spawn(async move {
            let mut prev = c.stats().udp_tx.bytes;
            while !m.closed.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let now = c.stats().udp_tx.bytes;
                eprintln!("[bw] tx {:.1} kB/s", (now - prev) as f64 / 1000.0);
                prev = now;
            }
        });
    }
    let newest_key = Arc::new(AtomicU32::new(0));
    let t0 = Instant::now();
    crate::rt::spawn(async move {
        let mut seq: u32 = 0;
        let mut need_key = false;
        while let Some(f) = rx.recv().await {
            if me.closed.load(Ordering::Relaxed) {
                break;
            }
            // Backed up far beyond what skipping at the encoder could absorb: drop deltas and
            // restart from a keyframe once it drains (the "auto" loop lowers the bitrate).
            // (Counting frames in flight would trip on any long round trip: at 60 fps and
            // 50 ms, three frames are always on their way.)
            if !f.key && (need_key || backlog(&pending, &conn) > BACKLOG_DROP) {
                crate::perf::tick("host-netdrop");
                need_key = true;
                if let Some(v) = me.video.lock().unwrap().as_ref() {
                    v.request_keyframe(); // the current one (the display may have been switched)
                }
                continue;
            }
            need_key = false;
            seq = seq.wrapping_add(1);
            crate::perf::stamp("host", seq);
            if f.key {
                newest_key.store(seq, Ordering::Relaxed);
            }
            let Ok(mut s) = conn.open_uni().await else { break };
            let _ = s.set_priority(seq as i32);
            let ts = (t0.elapsed().as_micros() / 1000) as u32 * 90; // 90 kHz like RTP
            let mut head = [0u8; 9];
            head[0] = f.key as u8;
            head[1..5].copy_from_slice(&ts.to_be_bytes());
            head[5..9].copy_from_slice(&seq.to_be_bytes());
            pending.lock().unwrap().insert(seq, Instant::now());
            let (inf, nk, my) = (pending.clone(), newest_key.clone(), seq);
            let key = f.key;
            if key {
                *me.key_in_flight.lock().unwrap() = Some(Instant::now());
            }
            let give_up = FRAME_GIVE_UP.max(Duration::from_secs_f64(f.data.len() as f64 * 8.0 / GIVE_UP_RATE));
            let me2 = me.clone();
            crate::rt::spawn(async move {
                let send = async {
                    let mut ok = s.write_all(&head).await.is_ok();
                    for chunk in f.data.chunks(64 * 1024) {
                        if !ok {
                            break;
                        }
                        if nk.load(Ordering::Relaxed) > my {
                            let _ = s.reset(0u32.into()); // superseded by a newer keyframe
                            ok = false;
                            break;
                        }
                        ok = s.write_all(chunk).await.is_ok();
                    }
                    if ok {
                        let _ = s.finish();
                        let _ = s.stopped().await;
                    }
                };
                if key {
                    send.await;
                    *me2.key_in_flight.lock().unwrap() = None;
                } else if tokio::time::timeout(give_up, send).await.is_err() {
                    // never delivered: give it up (the viewer resyncs on the next keyframe)
                    crate::perf::tick("host-frame-given-up");
                    let _ = s.reset(0u32.into());
                }
                inf.lock().unwrap().remove(&my);
            });
        }
    });
}

// ---- viewer: frames back in order; gaps resolved by jumping to a keyframe ----

type FrameTx = tokio::sync::mpsc::UnboundedSender<(u32, bool, u32, Vec<u8>)>;

/// Both sides: uni streams from the other side — screen frames (viewer, to `video`) and
/// camera frames.
fn start_uni_receiver(me: Arc<Session>, conn: quinn::Connection, video: Option<FrameTx>) {
    crate::rt::spawn(async move {
        while let Ok(mut s) = conn.accept_uni().await {
            let (tx, me) = (video.clone(), me.clone());
            crate::rt::spawn(async move {
                let mut head = [0u8; 9];
                if s.read_exact(&mut head).await.is_err() {
                    return;
                }
                let camera = head[0] & UNI_CAMERA != 0;
                let Ok(data) = s.read_to_end(if camera { 4 << 20 } else { 32 << 20 }).await else { return };
                let ts = u32::from_be_bytes(head[1..5].try_into().unwrap());
                let seq = u32::from_be_bytes(head[5..9].try_into().unwrap());
                let key = head[0] & 1 == 1;
                if camera {
                    me.camera_in(seq, key, data);
                } else if let Some(tx) = tx {
                    let _ = tx.send((seq, key, ts, data));
                }
            });
        }
    });
}

fn start_video_receiver(me: Arc<Session>, conn: quinn::Connection) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u32, bool, u32, Vec<u8>)>();
    start_uni_receiver(me.clone(), conn.clone(), Some(tx));
    crate::rt::spawn(async move {
        let mut next: Option<u32> = None; // next sequence number to deliver
        let mut waiting: BTreeMap<u32, (bool, u32, Vec<u8>)> = BTreeMap::new();
        let mut gap_since: Option<Instant> = None;
        let mut last_frame: Option<Instant> = None;
        let mut last_req = Instant::now() - Duration::from_secs(1);
        let ask_key = |last: &mut Instant| {
            if last.elapsed() > Duration::from_millis(300) {
                *last = Instant::now();
                let _ = conn.send_datagram(Bytes::from_static(&[DGRAM_KEYFRAME]));
            }
        };
        loop {
            let got = tokio::time::timeout(Duration::from_millis(40), rx.recv()).await;
            match got {
                Ok(Some((seq, key, ts, data))) => {
                    me.bytes_in.fetch_add(data.len() as u64, Ordering::Relaxed);
                    if next.is_none_or(|n| seq >= n) {
                        waiting.insert(seq, (key, ts, data));
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
            // A keyframe ahead makes everything before it unnecessary.
            if let Some((&k, _)) = waiting.iter().rev().find(|(s, f)| f.0 && next.is_none_or(|n| **s >= n)) {
                if next != Some(k) {
                    waiting = waiting.split_off(&k);
                    next = Some(k);
                }
            }
            if next.is_none() {
                if !waiting.is_empty() {
                    ask_key(&mut last_req);
                }
                waiting.clear();
                continue;
            }
            while let Some(n) = next {
                let Some((key, ts, data)) = waiting.remove(&n) else { break };
                me.frames_in.fetch_add(1, Ordering::Relaxed);
                crate::perf::tick("view-netrecv");
                if let Some(t) = last_frame.replace(Instant::now()) {
                    crate::perf::add("view-interval", t.elapsed());
                }
                crate::perf::stamp("view", n);
                (me.media)(Media::Video { key, ts, data });
                next = Some(n.wrapping_add(1));
                gap_since = None;
            }
            // A missing frame (its stream was reset or lost): wait briefly, then resync.
            if !waiting.is_empty() {
                let since = *gap_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_millis(150) {
                    waiting.clear();
                    next = None;
                    gap_since = None;
                    ask_key(&mut last_req);
                }
            }
        }
    });
}

// ---- connection setup ----

async fn establish(me: Arc<Session>, peer: Peer) -> Result<(), String> {
    let sock = me.sock.lock().unwrap().take().ok_or("already connecting")?;
    let token: [u8; 16] = hex::decode(&peer.token).ok().and_then(|t| t.try_into().ok()).ok_or("bad token")?;
    let peers: Vec<SocketAddr> = peer.cands.iter().filter_map(|c| c.parse().ok()).collect();
    // Short: on failure the viewer falls back to WebRTC (which can relay through TURN).
    let timeout = if me.host { Duration::from_secs(12) } else { Duration::from_secs(20) };
    let relay = me.relay.lock().unwrap().take();
    if let Some(r) = &relay {
        if let Err(e) = r.permit(&sock, &peers).await {
            eprintln!("[turn] permission: {e}");
        }
    }
    let (path, relayed) = net::punch(&sock, &peers, &token, me.host, timeout, relay.as_ref()).await.ok_or("noroute")?;
    *me.path.lock().unwrap() = Some(path);
    me.relayed.store(relayed, Ordering::Relaxed);
    let runtime = Arc::new(quinn::TokioRuntime);
    let socket: Arc<dyn quinn::AsyncUdpSocket> = match relay {
        Some(r) if relayed => {
            r.bind(&sock, path).await?;
            turn::RelaySocket::new(sock, r, path)
        }
        other => {
            // Direct path: the allocation is not needed any more (it times out on its own).
            drop(other);
            quinn::Runtime::wrap_udp_socket(&*runtime, sock.into_std().map_err(err)?).map_err(err)?
        }
    };
    let socket = match super::sim::Sim::from_env() {
        Some(sim) => super::sim::SimSocket::wrap(socket, sim),
        None => socket,
    };
    let conn = if me.host {
        let ep = quinn::Endpoint::new_with_abstract_socket(quinn::EndpointConfig::default(), Some(tls::server(&me.id, &peer.fp)?), socket, runtime).map_err(err)?;
        *me.endpoint.lock().unwrap() = Some(ep.clone());
        let incoming = tokio::time::timeout(Duration::from_secs(15), ep.accept()).await.map_err(|_| "noroute")?.ok_or("closed")?;
        incoming.await.map_err(err)?
    } else {
        let mut ep = quinn::Endpoint::new_with_abstract_socket(quinn::EndpointConfig::default(), None, socket, runtime).map_err(err)?;
        ep.set_default_client_config(tls::client(&me.id, &peer.fp)?);
        *me.endpoint.lock().unwrap() = Some(ep.clone());
        let connecting = ep.connect(path, "manydesk").map_err(err)?;
        tokio::time::timeout(Duration::from_secs(15), connecting).await.map_err(|_| "noroute")?.map_err(err)?
    };
    *me.conn.lock().unwrap() = Some(conn.clone());

    // Control and file streams: opened by the viewer, tagged with their first byte.
    if me.host {
        for _ in 0..2 {
            let (tx, mut rx) = conn.accept_bi().await.map_err(err)?;
            let mut tag = [0u8; 1];
            rx.read_exact(&mut tag).await.map_err(err)?;
            let slot = if tag[0] == CH_CTL { &me.ctl } else { &me.file };
            *slot.lock().await = Some(tx);
            crate::rt::spawn(read_msgs(me.clone(), rx, tag[0]));
        }
    } else {
        for tag in [CH_CTL, CH_FILE] {
            let (mut tx, rx) = conn.open_bi().await.map_err(err)?;
            tx.write_all(&[tag]).await.map_err(err)?;
            let slot = if tag == CH_CTL { &me.ctl } else { &me.file };
            *slot.lock().await = Some(tx);
            crate::rt::spawn(read_msgs(me.clone(), rx, tag));
        }
    }

    // Later bi streams are TCP tunnels.
    if me.host {
        let (c, me2) = (conn.clone(), me.clone());
        crate::rt::spawn(async move {
            while let Ok((tx, rx)) = c.accept_bi().await {
                crate::rt::spawn(serve_tunnel(me2.clone(), tx, rx));
            }
        });
    }

    if me.host && !me.hold.load(Ordering::SeqCst) {
        me.start_sending();
    }
    if me.host {
        start_uni_receiver(me.clone(), conn.clone(), None);
    } else {
        start_video_receiver(me.clone(), conn.clone());
    }
    let (c, rx_me) = (conn.clone(), me.clone());
    crate::rt::spawn(async move {
        while let Ok(d) = c.read_datagram().await {
            match d.first() {
                Some(&DGRAM_KEYFRAME) => {
                    // One keyframe at a time: on a slow link it can take seconds to arrive,
                    // and a new one would reset it before it does — no picture ever.
                    let busy = rx_me.key_in_flight.lock().unwrap().is_some_and(|t| t.elapsed() < KEY_WAIT);
                    if !busy {
                        if let Some(v) = rx_me.video.lock().unwrap().as_ref() {
                            v.request_keyframe();
                        }
                    }
                }
                Some(&DGRAM_AUDIO) => (rx_me.media)(Media::Audio(d[1..].to_vec())),
                Some(&DGRAM_CAM_KEY) => rx_me.cam_key.store(true, Ordering::Relaxed),
                _ => {}
            }
        }
    });

    me.emit(json!({ "e": "open", "path": path.to_string(), "lan": !relayed && net::is_private(path.ip()), "relay": relayed }));
    let watch = me.clone();
    crate::rt::spawn(async move {
        let reason = conn.closed().await;
        eprintln!("[session] connection ended: {reason}");
        let r = match reason {
            quinn::ConnectionError::ApplicationClosed(_) | quinn::ConnectionError::LocallyClosed => "bye",
            quinn::ConnectionError::TimedOut => "lost",
            _ => "lost",
        };
        if let Some(v) = watch.video.lock().unwrap().as_ref() {
            v.stop();
        }
        watch.fail(r);
    });
    Ok(())
}

// ---- API ----

impl Session {
    /// Binds the socket and gathers candidates. Returns (session, {cands, fp, token}).
    pub async fn prepare(host: bool, stun: Vec<String>, turn: Option<turn::Server>, events: OnEvent, media: OnMedia) -> Result<(Arc<Session>, Value), String> {
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(err)?;
        let mut cands = net::gather(&sock, &stun).await;
        let mut relay = None;
        if let Some(t) = turn {
            match tokio::time::timeout(Duration::from_secs(6), turn::Relay::allocate(&sock, &t)).await {
                Ok(Ok(r)) => {
                    cands.push(r.relayed);
                    relay = Some(r);
                }
                Ok(Err(e)) => eprintln!("[turn] {e}"),
                Err(_) => eprintln!("[turn] no answer from {}", t.url),
            }
        }
        let identity = tls::Identity::new()?;
        let token: [u8; 16] = rand::random();
        let out = json!({
            "cands": cands.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            "fp": identity.fp, "token": hex::encode(token),
        });
        let s = Arc::new(Session {
            host,
            events,
            media,
            id: identity,
            token,
            sock: Mutex::new(Some(sock)),
            endpoint: Mutex::new(None),
            conn: Mutex::new(None),
            ctl: AsyncMutex::new(None),
            file: AsyncMutex::new(None),
            file_turn: AsyncMutex::new(()),
            video: Mutex::new(None),
            sink: Mutex::new(None),
            hold: AtomicBool::new(false),
            sending: AtomicBool::new(false),
            peer_fp: Mutex::new(String::new()),
            path: Mutex::new(None),
            relay: Mutex::new(relay),
            relayed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            tunnel_ok: AtomicBool::new(false),
            tunnels: AtomicUsize::new(0),
            key_in_flight: Mutex::new(None),
            frames_in: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            cursor_apart: AtomicBool::new(false),
            camera_ok: AtomicBool::new(false),
            cam_seq: AtomicU32::new(0),
            cam_key: Arc::new(AtomicBool::new(false)),
            cam_rx: Mutex::new((None, None)),
        });
        Ok((s, out))
    }

    pub fn fingerprint(&self) -> &str {
        &self.id.fp
    }

    /// Host: which screen to send (before connecting). Returns its pixel size.
    pub async fn add_video(&self, screen: Option<u32>) -> (u32, u32) {
        let screens = crate::rt::spawn_blocking(|| capture::screens(false)).await.unwrap_or_default();
        let pick = screens.iter().find(|x| Some(x.id) == screen).or_else(|| screens.first()).cloned();
        let video = Video::new(pick);
        let size = video.size();
        *self.video.lock().unwrap() = Some(video);
        size
    }

    /// Host: shows another display from now on (the viewer sees a keyframe of the new
    /// size). Returns the display, or None when it no longer exists.
    pub async fn switch_screen(&self, id: u32) -> Option<capture::Screen> {
        let screens = crate::rt::spawn_blocking(|| capture::screens(false)).await.unwrap_or_default();
        let screen = screens.into_iter().find(|x| x.id == id)?;
        let video = Video::new(Some(screen.clone()));
        let old = self.video.lock().unwrap().replace(video.clone());
        if let Some(old) = old {
            let p = old.params();
            video.set_params(p.scale, p.fps, p.bitrate);
            video.set_cursor_in_picture(old.cursor_in_picture());
            old.stop();
        }
        if let Some(sink) = self.sink.lock().unwrap().clone() {
            video.start(sink);
        }
        Some(screen)
    }

    /// Host: keep the picture back until `release_video` (unattended access, before the
    /// password is checked). Call before `connect`.
    pub fn hold_video(&self) {
        self.hold.store(true, Ordering::SeqCst);
    }

    /// Host: start sending the picture (now, or as soon as the connection is up).
    pub fn release_video(self: &Arc<Self>) {
        self.hold.store(false, Ordering::SeqCst);
        if self.conn().is_some() {
            self.start_sending();
        }
    }

    fn start_sending(self: &Arc<Self>) {
        let (Some(conn), Some(v)) = (self.conn(), self.video.lock().unwrap().clone()) else { return };
        if !self.sending.swap(true, Ordering::SeqCst) {
            start_video_sender(self.clone(), conn, v);
        }
    }

    /// The other side's certificate fingerprint.
    pub fn peer_fingerprint(&self) -> String {
        self.peer_fp.lock().unwrap().clone()
    }

    /// Starts punching toward the peer and brings up QUIC; the outcome arrives as an event.
    /// The host uses its own token (the viewer echoes the host's from the offer).
    pub fn connect(self: &Arc<Self>, mut peer: Peer) {
        *self.peer_fp.lock().unwrap() = peer.fp.clone();
        if self.host {
            peer.token = hex::encode(self.token);
        }
        let s = self.clone();
        crate::rt::spawn(async move {
            if let Err(e) = establish(s.clone(), peer).await {
                s.fail(&e);
            }
        });
    }

    pub async fn send_text(&self, file_channel: bool, text: &str) -> Result<(), String> {
        write_msg(if file_channel { &self.file } else { &self.ctl }, 0, text.as_bytes()).await
    }

    pub async fn send_file_bytes(&self, data: &[u8]) -> Result<(), String> {
        write_msg(&self.file, 1, data).await
    }

    /// Host: the viewer draws the pointer itself. From now on it leaves the picture (no
    /// frame for a pointer move, no compression artifacts on it) and its position and
    /// shape (on change only) go over the control stream as
    /// {"t":"cur","x","y"} (0..1 on the display; {"t":"cur"} alone: not on it) and
    /// {"t":"curimg","png","w","h","hx","hy"} (size and hot spot in display pixels).
    /// macOS and Windows hosts; elsewhere the pointer stays in the picture. (Windows'
    /// desktop duplication leaves the pointer out of the picture anyway.)
    pub fn cursor_apart(self: &Arc<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        {
            if self.cursor_apart.swap(true, Ordering::SeqCst) {
                return;
            }
            let me = self.clone();
            crate::rt::spawn(async move {
                use base64::Engine;
                let mut last_pos: Option<Option<(f64, f64)>> = None;
                let mut last_shape = 0u64;
                let mut n = 0u32;
                while !me.closed.load(Ordering::Relaxed) {
                    let video = me.video.lock().unwrap().clone();
                    let Some(id) = video.as_ref().and_then(|v| {
                        v.set_cursor_in_picture(false);
                        v.screen().map(|s| s.id)
                    }) else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    // position at 60 Hz, shape every 3rd look
                    #[cfg(target_os = "macos")]
                    let (pos, shape) = crate::media::mac::cursor(id, n % 3 == 0);
                    #[cfg(windows)]
                    let (pos, shape) = crate::media::wincap::cursor(id, n % 3 == 0);
                    n = n.wrapping_add(1);
                    let pos = pos.map(|(x, y)| ((x * 1e4).round() / 1e4, (y * 1e4).round() / 1e4));
                    let mut msgs = Vec::new();
                    if let Some(c) = shape {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        (&c.rgba, c.px, c.w.to_bits(), c.hx.to_bits(), c.hy.to_bits()).hash(&mut h);
                        let hash = h.finish();
                        if hash != last_shape {
                            last_shape = hash;
                            let mut png = Vec::new();
                            if let Some(img) = image::RgbaImage::from_raw(c.px as u32, c.py as u32, c.rgba) {
                                let _ = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
                            }
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
                            msgs.push(json!({ "t": "curimg", "png": b64, "w": c.w, "h": c.h, "hx": c.hx, "hy": c.hy }));
                        }
                    }
                    if last_pos != Some(pos) {
                        last_pos = Some(pos);
                        msgs.push(match pos {
                            Some((x, y)) => json!({ "t": "cur", "x": x, "y": y }),
                            None => json!({ "t": "cur" }),
                        });
                    }
                    for m in msgs {
                        if me.send_text(false, &m.to_string()).await.is_err() {
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(16)).await;
                }
            });
        }
    }

    /// Host: whether the viewer may open TCP tunnels.
    pub fn allow_tunnels(&self, on: bool) {
        self.tunnel_ok.store(on, Ordering::SeqCst);
    }

    /// Viewer: one TCP connection to `host:port` as seen from the host computer.
    pub async fn open_tunnel(&self, host: &str, port: u16) -> Result<(quinn::SendStream, quinn::RecvStream), String> {
        let conn = self.conn.lock().unwrap().clone().ok_or("not connected")?;
        let (mut tx, mut rx) = conn.open_bi().await.map_err(err)?;
        let head = json!({ "h": host, "p": port }).to_string();
        let mut msg = vec![CH_TUNNEL];
        msg.extend_from_slice(&(head.len() as u16).to_be_bytes());
        msg.extend_from_slice(head.as_bytes());
        tx.write_all(&msg).await.map_err(err)?;
        let mut ok = [1u8; 1];
        rx.read_exact(&mut ok).await.map_err(err)?;
        if ok[0] != 0 {
            return Err(match ok[0] {
                2 => "denied".into(),
                _ => "refused".into(),
            });
        }
        Ok((tx, rx))
    }

    /// Held while sending one file, so concurrent transfers don't interleave their chunks.
    pub async fn file_turn(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.file_turn.lock().await
    }

    pub fn video_params(&self, scale: f64, fps: f64, bitrate: u32) {
        if let Some(v) = self.video.lock().unwrap().as_ref() {
            v.set_params(scale, fps, bitrate);
        }
    }

    pub fn send_audio(&self, opus: &[u8]) {
        if let Some(c) = self.conn() {
            let mut d = Vec::with_capacity(opus.len() + 1);
            d.push(DGRAM_AUDIO);
            d.extend_from_slice(opus);
            let _ = c.send_datagram(Bytes::from(d));
        }
    }

    /// Host: whether the viewer may show its camera here.
    pub fn allow_camera(&self, on: bool) {
        self.camera_ok.store(on, Ordering::SeqCst);
    }

    /// Set when the other side asks for a keyframe of our camera (the encoder clears it).
    pub fn camera_keyframe(&self) -> Arc<AtomicBool> {
        self.cam_key.clone()
    }

    /// One encoded frame of our camera, on a uni stream of its own. Small and frequent:
    /// a frame not delivered within a second is given up (a keyframe follows on request).
    pub fn send_camera(&self, f: EncodedFrame) {
        let Some(conn) = self.conn() else { return };
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let seq = self.cam_seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        crate::rt::spawn(async move {
            let Ok(mut s) = conn.open_uni().await else { return };
            let mut head = [0u8; 9];
            head[0] = UNI_CAMERA | f.key as u8;
            head[5..9].copy_from_slice(&seq.to_be_bytes());
            let send = async {
                if s.write_all(&head).await.is_ok() && s.write_all(&f.data).await.is_ok() {
                    let _ = s.finish();
                    let _ = s.stopped().await;
                }
            };
            let wait = if f.key { Duration::from_secs(5) } else { Duration::from_secs(1) };
            if tokio::time::timeout(wait, send).await.is_err() {
                let _ = s.reset(0u32.into());
            }
        });
    }

    /// A camera frame from the other side: delivered in order; after a gap, deltas wait
    /// for the next keyframe (asked for at most every 300 ms).
    fn camera_in(&self, seq: u32, key: bool, data: Vec<u8>) {
        if self.host && !self.camera_ok.load(Ordering::SeqCst) {
            return;
        }
        let mut rx = self.cam_rx.lock().unwrap();
        if !key && rx.0 != Some(seq) {
            // a late frame from before the one just shown: nothing lost
            if rx.0.is_some_and(|n| seq < n && n - seq < 1000) {
                return;
            }
            rx.0 = None;
            if rx.1.is_none_or(|t| t.elapsed() > Duration::from_millis(300)) {
                rx.1 = Some(Instant::now());
                self.request_camera_keyframe();
            }
            return;
        }
        rx.0 = Some(seq.wrapping_add(1));
        drop(rx);
        (self.media)(Media::Camera { key, data });
    }

    /// The camera decoder lost sync: ask the other side for a keyframe.
    pub fn request_camera_keyframe(&self) {
        if let Some(c) = self.conn() {
            let _ = c.send_datagram(Bytes::from_static(&[DGRAM_CAM_KEY]));
        }
    }

    /// Viewer: the decoder lost sync, ask for a keyframe.
    pub fn request_keyframe(&self) {
        if let Some(c) = self.conn() {
            let _ = c.send_datagram(Bytes::from_static(&[DGRAM_KEYFRAME]));
        }
    }

    pub fn stats(&self) -> Value {
        let path = *self.path.lock().unwrap();
        let mut out = json!({
            "path": path.map(|p| p.to_string()), "relay": self.relayed.load(Ordering::Relaxed),
            "lan": !self.relayed.load(Ordering::Relaxed) && path.map(|p| net::is_private(p.ip())).unwrap_or(false),
            "framesIn": self.frames_in.load(Ordering::Relaxed), "bytesIn": self.bytes_in.load(Ordering::Relaxed),
        });
        if let Some(c) = self.conn() {
            let cs = c.stats();
            out["rtt"] = json!(cs.path.rtt.as_secs_f64() * 1000.0);
            out["cwnd"] = json!(cs.path.cwnd);
            out["lost"] = json!(cs.path.lost_packets);
            out["sent"] = json!(cs.udp_tx.bytes);
            out["recv"] = json!(cs.udp_rx.bytes);
        }
        if let Some(v) = self.video.lock().unwrap().as_ref() {
            out["video"] = v.stats();
        }
        out
    }

    pub fn is_host(&self) -> bool {
        self.host
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(v) = self.video.lock().unwrap().take() {
            v.stop();
        }
        if let Some(c) = self.conn() {
            c.close(0u32.into(), b"bye");
        }
        if let Some(ep) = self.endpoint.lock().unwrap().take() {
            crate::rt::spawn(async move {
                tokio::time::timeout(Duration::from_millis(300), ep.wait_idle()).await.ok();
            });
        }
    }
}
