// MDP — ManyDesk's native protocol between two current-version peers (like AnyDesk's
// own transport): QUIC (TLS 1.3, BBR) over a hole-punched UDP socket.
//
//   viewer = QUIC client, host = QUIC server; certificates pinned both ways (tls.rs)
//   bi stream 'c': control messages        bi stream 'f': file transfer
//   uni stream per video frame (host → viewer): [key u8][ts u32][seq u32] + Annex-B H.264
//   datagram [1] (viewer → host): keyframe request
//   datagram [2] + Opus packet (both ways): voice, 20 ms per packet
//
// Each frame gets its own stream, so a lost packet only delays that frame; once a newer
// keyframe exists, older frames still in flight are reset instead of delivered late.
// Older peers (WebRTC) are handled by the renderer's legacy path.
mod net;
mod tls;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::Mutex as AsyncMutex;

use crate::media::capture;
use crate::media::encode::{EncodedFrame, Sink, Video};

const CH_CTL: u8 = b'c';
const CH_FILE: u8 = b'f';
const DGRAM_KEYFRAME: u8 = 1;
const DGRAM_AUDIO: u8 = 2;
// Binary frames to the renderer (same layout as the Linux WebRTC bridge).
const MEDIA_VIDEO: u8 = 1;
const MEDIA_FILE: u8 = 2;
const MEDIA_AUDIO: u8 = 3;

#[derive(Deserialize)]
pub struct Peer {
    cands: Vec<String>,
    fp: String,
    token: String,
}

struct Session {
    host: bool,
    events: Channel<Value>,
    media: Channel<InvokeResponseBody>,
    id: tls::Identity,
    token: [u8; 16],
    sock: Mutex<Option<tokio::net::UdpSocket>>,
    endpoint: Mutex<Option<quinn::Endpoint>>,
    conn: Mutex<Option<quinn::Connection>>,
    ctl: AsyncMutex<Option<quinn::SendStream>>,
    file: AsyncMutex<Option<quinn::SendStream>>,
    video: Mutex<Option<Arc<Video>>>,
    path: Mutex<Option<SocketAddr>>,
    closed: AtomicBool,
    frames_in: AtomicU64,
    bytes_in: AtomicU64,
}

impl Session {
    fn emit(&self, v: Value) {
        let _ = self.events.send(v);
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

#[derive(Default)]
pub struct Mdp {
    sessions: Mutex<HashMap<u32, Arc<Session>>>,
}

impl Mdp {
    fn get(&self, id: u32) -> Result<Arc<Session>, String> {
        self.sessions.lock().unwrap().get(&id).cloned().ok_or_else(|| "no such session".into())
    }
}

type St<'a> = tauri::State<'a, Mdp>;

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
            _ => {
                let mut out = Vec::with_capacity(data.len() + 6);
                out.push(MEDIA_FILE);
                out.push(4);
                out.extend_from_slice(b"file");
                out.extend_from_slice(&data);
                let _ = me.media.send(InvokeResponseBody::Raw(out));
            }
        }
    }
}

// ---- host: encoded frames → one QUIC uni stream each ----

struct QuicSink {
    tx: tokio::sync::mpsc::Sender<EncodedFrame>,
}

impl Sink for QuicSink {
    fn push(&self, f: EncodedFrame) -> bool {
        self.tx.try_send(f).is_ok()
    }
    fn closed(&self) -> bool {
        self.tx.is_closed()
    }
}

fn start_video_sender(me: Arc<Session>, conn: quinn::Connection, video: Arc<Video>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EncodedFrame>(2);
    video.start(Arc::new(QuicSink { tx }));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let newest_key = Arc::new(AtomicU32::new(0));
    let t0 = Instant::now();
    tauri::async_runtime::spawn(async move {
        let mut seq: u32 = 0;
        let mut need_key = false;
        while let Some(f) = rx.recv().await {
            if me.closed.load(Ordering::Relaxed) {
                break;
            }
            // More than a few frames queued: the path is saturated. Drop deltas and
            // restart from a keyframe once it drains (the "auto" loop lowers the bitrate).
            if !f.key && (need_key || in_flight.load(Ordering::Relaxed) >= 3) {
                need_key = true;
                video.request_keyframe();
                continue;
            }
            need_key = false;
            seq = seq.wrapping_add(1);
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
            in_flight.fetch_add(1, Ordering::Relaxed);
            let (inf, nk, my) = (in_flight.clone(), newest_key.clone(), seq);
            tauri::async_runtime::spawn(async move {
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
                inf.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
}

// ---- viewer: frames back in order; gaps resolved by jumping to a keyframe ----

fn start_video_receiver(me: Arc<Session>, conn: quinn::Connection) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u32, bool, u32, Vec<u8>)>();
    let c = conn.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(mut s) = c.accept_uni().await {
            let tx = tx.clone();
            tauri::async_runtime::spawn(async move {
                let mut head = [0u8; 9];
                if s.read_exact(&mut head).await.is_err() {
                    return;
                }
                let Ok(data) = s.read_to_end(32 << 20).await else { return };
                let ts = u32::from_be_bytes(head[1..5].try_into().unwrap());
                let seq = u32::from_be_bytes(head[5..9].try_into().unwrap());
                let _ = tx.send((seq, head[0] == 1, ts, data));
            });
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut next: Option<u32> = None; // next sequence number to deliver
        let mut waiting: BTreeMap<u32, (bool, u32, Vec<u8>)> = BTreeMap::new();
        let mut gap_since: Option<Instant> = None;
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
                let mut out = Vec::with_capacity(data.len() + 6);
                out.push(MEDIA_VIDEO);
                out.push(key as u8);
                out.extend_from_slice(&ts.to_be_bytes());
                out.extend_from_slice(&data);
                me.frames_in.fetch_add(1, Ordering::Relaxed);
                let _ = me.media.send(InvokeResponseBody::Raw(out));
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
    let path = net::punch(&sock, &peers, &token, me.host, timeout).await.ok_or("noroute")?;
    *me.path.lock().unwrap() = Some(path);
    let std_sock = sock.into_std().map_err(err)?;
    let runtime = Arc::new(quinn::TokioRuntime);
    let conn = if me.host {
        let ep = quinn::Endpoint::new(quinn::EndpointConfig::default(), Some(tls::server(&me.id, &peer.fp)?), std_sock, runtime).map_err(err)?;
        *me.endpoint.lock().unwrap() = Some(ep.clone());
        let incoming = tokio::time::timeout(Duration::from_secs(15), ep.accept()).await.map_err(|_| "noroute")?.ok_or("closed")?;
        incoming.await.map_err(err)?
    } else {
        let mut ep = quinn::Endpoint::new(quinn::EndpointConfig::default(), None, std_sock, runtime).map_err(err)?;
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
            tauri::async_runtime::spawn(read_msgs(me.clone(), rx, tag[0]));
        }
    } else {
        for tag in [CH_CTL, CH_FILE] {
            let (mut tx, rx) = conn.open_bi().await.map_err(err)?;
            tx.write_all(&[tag]).await.map_err(err)?;
            let slot = if tag == CH_CTL { &me.ctl } else { &me.file };
            *slot.lock().await = Some(tx);
            tauri::async_runtime::spawn(read_msgs(me.clone(), rx, tag));
        }
    }

    let video = if me.host { me.video.lock().unwrap().clone() } else { None };
    if let Some(v) = &video {
        start_video_sender(me.clone(), conn.clone(), v.clone());
    }
    if !me.host {
        start_video_receiver(me.clone(), conn.clone());
    }
    let (c, rx_me) = (conn.clone(), me.clone());
    tauri::async_runtime::spawn(async move {
        while let Ok(d) = c.read_datagram().await {
            match d.first() {
                Some(&DGRAM_KEYFRAME) => {
                    if let Some(v) = &video {
                        v.request_keyframe();
                    }
                }
                Some(&DGRAM_AUDIO) => {
                    let mut out = Vec::with_capacity(d.len());
                    out.push(MEDIA_AUDIO);
                    out.extend_from_slice(&d[1..]);
                    let _ = rx_me.media.send(InvokeResponseBody::Raw(out));
                }
                _ => {}
            }
        }
    });

    me.emit(json!({ "e": "open", "path": path.to_string(), "lan": net::is_private(path.ip()) }));
    let watch = me.clone();
    tauri::async_runtime::spawn(async move {
        let reason = conn.closed().await;
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

// ---- commands ----

#[tauri::command]
pub async fn mdp_prepare(st: St<'_>, id: u32, host: bool, stun: Vec<String>, events: Channel<Value>, media: Channel<InvokeResponseBody>) -> Result<Value, String> {
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(err)?;
    let cands = net::gather(&sock, &stun).await;
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
        video: Mutex::new(None),
        path: Mutex::new(None),
        closed: AtomicBool::new(false),
        frames_in: AtomicU64::new(0),
        bytes_in: AtomicU64::new(0),
    });
    st.sessions.lock().unwrap().insert(id, s);
    Ok(out)
}

/// Host: which screen to send (before connecting).
#[tauri::command]
pub async fn mdp_add_video(st: St<'_>, id: u32, screen: Option<u32>) -> Result<Value, String> {
    let s = st.get(id)?;
    let screens = tauri::async_runtime::spawn_blocking(capture::screens).await.unwrap_or_default();
    let pick = screens.iter().find(|x| Some(x.id) == screen).or_else(|| screens.first()).cloned();
    let video = Video::new(pick);
    let (w, h) = video.size();
    *s.video.lock().unwrap() = Some(video);
    Ok(json!({ "width": w, "height": h }))
}

/// Starts punching toward the peer and brings up QUIC; the outcome arrives as an event.
/// The host passes its own token (the viewer echoes the host's from the offer).
#[tauri::command]
pub async fn mdp_connect(st: St<'_>, id: u32, peer: Peer) -> Result<(), String> {
    let s = st.get(id)?;
    let mut peer = peer;
    if s.host {
        peer.token = hex::encode(s.token);
    }
    tauri::async_runtime::spawn(async move {
        if let Err(e) = establish(s.clone(), peer).await {
            s.fail(&e);
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn mdp_send(st: St<'_>, id: u32, chan: String, text: String) -> Result<(), String> {
    let s = st.get(id)?;
    let stream = if chan == "file" { &s.file } else { &s.ctl };
    write_msg(stream, 0, text.as_bytes()).await
}

/// Binary file data: raw request body, `x-id` header.
#[tauri::command]
pub async fn mdp_send_bin(st: St<'_>, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else { return Err("expected bytes".into()) };
    let id: u32 = request.headers().get("x-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or("x-id")?;
    let s = st.get(id)?;
    write_msg(&s.file, 1, body).await
}

#[tauri::command]
pub async fn mdp_video_params(st: St<'_>, id: u32, scale: f64, fps: f64, bitrate: u32) -> Result<(), String> {
    if let Some(v) = st.get(id)?.video.lock().unwrap().as_ref() {
        v.set_params(scale, fps, bitrate);
    }
    Ok(())
}

/// One Opus packet from the microphone (raw body, `x-id` header).
#[tauri::command]
pub async fn mdp_audio(st: St<'_>, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else { return Err("expected bytes".into()) };
    let id: u32 = request.headers().get("x-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or("x-id")?;
    if let Some(c) = st.get(id)?.conn() {
        let mut d = Vec::with_capacity(body.len() + 1);
        d.push(DGRAM_AUDIO);
        d.extend_from_slice(body);
        let _ = c.send_datagram(Bytes::from(d));
    }
    Ok(())
}

/// Viewer: the decoder lost sync, ask for a keyframe.
#[tauri::command]
pub async fn mdp_keyframe(st: St<'_>, id: u32) -> Result<(), String> {
    if let Some(c) = st.get(id)?.conn() {
        let _ = c.send_datagram(Bytes::from_static(&[DGRAM_KEYFRAME]));
    }
    Ok(())
}

#[tauri::command]
pub async fn mdp_stats(st: St<'_>, id: u32) -> Result<Value, String> {
    let s = st.get(id)?;
    let path = *s.path.lock().unwrap();
    let mut out = json!({
        "path": path.map(|p| p.to_string()), "lan": path.map(|p| net::is_private(p.ip())).unwrap_or(false),
        "framesIn": s.frames_in.load(Ordering::Relaxed), "bytesIn": s.bytes_in.load(Ordering::Relaxed),
    });
    if let Some(c) = s.conn() {
        let cs = c.stats();
        out["rtt"] = json!(cs.path.rtt.as_secs_f64() * 1000.0);
        out["cwnd"] = json!(cs.path.cwnd);
        out["lost"] = json!(cs.path.lost_packets);
        out["sent"] = json!(cs.udp_tx.bytes);
        out["recv"] = json!(cs.udp_rx.bytes);
    }
    if let Some(v) = s.video.lock().unwrap().as_ref() {
        out["video"] = v.stats();
    }
    Ok(out)
}

#[tauri::command]
pub async fn mdp_close(st: St<'_>, id: u32) -> Result<(), String> {
    let s = st.sessions.lock().unwrap().remove(&id);
    if let Some(s) = s {
        s.closed.store(true, Ordering::SeqCst);
        if let Some(v) = s.video.lock().unwrap().take() {
            v.stop();
        }
        if let Some(c) = s.conn() {
            c.close(0u32.into(), b"bye");
        }
        let ep = s.endpoint.lock().unwrap().take();
        if let Some(ep) = ep {
            tokio::time::timeout(Duration::from_millis(300), ep.wait_idle()).await.ok();
        }
    }
    Ok(())
}
