// Decentralized rendezvous over two independent public networks (Nostr relays and
// MQTT brokers), wire-compatible with the Electron build (src/main/rendezvous.js).
//
// Only the short connection handshake travels here; screen, input, voice and files
// flow peer-to-peer. Every message is a Schnorr-signed Nostr event (throwaway per-run
// key) verified on receipt, so neither relays nor brokers can forge or alter it.
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use rand::RngCore;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use crate::bus::Emit;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const KIND: u64 = 21059; // ephemeral: relays forward but do not store
const CLOCK_SKEW_S: i64 = 600;
const MQTT_PREFIX: &str = "manydesk/v1/";

pub const RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://relay.primal.net",
    "wss://offchain.pub",
    "wss://relay.nostr.net",
    "wss://relay.snort.social",
    "wss://nostr-pub.wellorder.net",
    "wss://nostr.oxtr.dev",
    "wss://nostr.bitcoiner.social",
    "wss://relay.coinos.io",
    "wss://yabu.me",
    "wss://nostr.sathoarder.com",
];
pub const BROKERS: &[&str] = &[
    "wss://broker.hivemq.com:8884/mqtt",
    "wss://broker.emqx.io:8084/mqtt",
    "wss://broker-cn.emqx.io:8084/mqtt",
    "wss://test.mosquitto.org:8081/mqtt",
];

#[derive(Clone)]
struct Sub {
    id: String,
    live_only: bool,
}

struct Endpoint {
    url: String,
    kind: &'static str,
    open: AtomicBool,
    tx: Mutex<Option<UnboundedSender<Message>>>,
}

#[derive(Serialize, Clone)]
pub struct EndpointStatus {
    url: String,
    kind: &'static str,
    open: bool,
}

#[derive(Serialize, Clone)]
pub struct Status {
    connected: usize,
    total: usize,
    relays: Vec<EndpointStatus>,
    #[serde(rename = "clockOffsetS")]
    clock_offset_s: i64,
}

#[derive(Serialize, Clone)]
pub struct PublishResult {
    ok: usize,
    sent: usize,
    relays: usize,
    brokers: usize,
}

#[derive(Serialize, Clone)]
struct Incoming {
    topic: String,
    content: String,
    pubkey: String,
}

pub struct Rendezvous {
    emit: Emit,
    sk: SigningKey,
    pubkey: String,
    subs: Mutex<HashMap<String, Sub>>,
    endpoints: Vec<Arc<Endpoint>>,
    seen: Mutex<HashSet<String>>,
    acks: Mutex<HashMap<String, UnboundedSender<bool>>>,
    clock_offset_ms: AtomicI64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

// Same serialisation as the JS side: JSON.stringify([0, pubkey, created_at, kind, tags, content]).
fn event_id(ev: &Value) -> String {
    let arr = json!([0, ev["pubkey"], ev["created_at"], ev["kind"], ev["tags"], ev["content"]]);
    hex::encode(Sha256::digest(serde_json::to_string(&arr).unwrap_or_default().as_bytes()))
}

impl Rendezvous {
    pub fn new(emit: Emit) -> Arc<Self> {
        let sk = SigningKey::random(&mut rand::thread_rng());
        let pubkey = hex::encode(sk.verifying_key().to_bytes());
        let endpoints = RELAYS
            .iter()
            .map(|u| (u, "nostr"))
            .chain(BROKERS.iter().map(|u| (u, "mqtt")))
            .map(|(u, k)| Arc::new(Endpoint { url: u.to_string(), kind: k, open: AtomicBool::new(false), tx: Mutex::new(None) }))
            .collect();
        Arc::new(Self {
            emit,
            sk,
            pubkey,
            subs: Mutex::new(HashMap::new()),
            endpoints,
            seen: Mutex::new(HashSet::new()),
            acks: Mutex::new(HashMap::new()),
            clock_offset_ms: AtomicI64::new(0),
        })
    }

    fn now(&self) -> i64 {
        now_ms() + self.clock_offset_ms.load(Ordering::Relaxed)
    }

    pub fn start(self: &Arc<Self>) {
        for ep in &self.endpoints {
            let me = self.clone();
            let ep = ep.clone();
            crate::rt::spawn(async move {
                if ep.kind == "nostr" { me.run_relay(ep).await } else { me.run_broker(ep).await }
            });
        }
        let me = self.clone();
        crate::rt::spawn(async move {
            loop {
                if let Some(off) = sync_clock().await {
                    me.clock_offset_ms.store(off, Ordering::Relaxed);
                    me.emit_status();
                }
                tokio::time::sleep(Duration::from_secs(1800)).await;
            }
        });
    }

    pub fn status(&self) -> Status {
        let relays: Vec<EndpointStatus> = self
            .endpoints
            .iter()
            .map(|e| EndpointStatus { url: e.url.clone(), kind: e.kind, open: e.open.load(Ordering::Relaxed) })
            .collect();
        Status {
            connected: relays.iter().filter(|r| r.open).count(),
            total: relays.len(),
            relays,
            clock_offset_s: self.clock_offset_ms.load(Ordering::Relaxed) / 1000,
        }
    }

    fn emit_status(&self) {
        (self.emit)("rv:status", serde_json::to_value(self.status()).unwrap_or_default());
    }

    fn send_to(ep: &Endpoint, msg: Message) {
        if ep.open.load(Ordering::Relaxed) {
            if let Some(tx) = ep.tx.lock().unwrap().as_ref() {
                let _ = tx.send(msg);
            }
        }
    }

    fn req_json(&self, topic: &str, sub: &Sub) -> String {
        json!(["REQ", sub.id, { "kinds": [KIND], "#t": [topic], "since": self.now() / 1000 - CLOCK_SKEW_S }]).to_string()
    }

    // live_only: the long-lived ID topic ignores anything a relay had stored (EOSE).
    pub fn subscribe(&self, topic: &str, live_only: bool) {
        let sub = {
            let mut subs = self.subs.lock().unwrap();
            if subs.contains_key(topic) {
                return;
            }
            let sub = Sub { id: format!("md{}", rand_hex(6)), live_only };
            subs.insert(topic.to_string(), sub.clone());
            sub
        };
        for ep in &self.endpoints {
            if ep.kind == "nostr" {
                Self::send_to(ep, Message::text(self.req_json(topic, &sub)));
            } else {
                Self::send_to(ep, Message::binary(mqtt_subscribe(&format!("{MQTT_PREFIX}{topic}"))));
            }
        }
    }

    pub fn unsubscribe(&self, topic: &str) {
        let Some(sub) = self.subs.lock().unwrap().remove(topic) else { return };
        for ep in &self.endpoints {
            if ep.kind == "nostr" {
                Self::send_to(ep, Message::text(json!(["CLOSE", sub.id]).to_string()));
            } else {
                Self::send_to(ep, Message::binary(mqtt_unsubscribe(&format!("{MQTT_PREFIX}{topic}"))));
            }
        }
    }

    pub async fn publish(&self, topic: &str, content: &str) -> PublishResult {
        let mut ev = json!({
            "pubkey": self.pubkey, "created_at": self.now() / 1000, "kind": KIND,
            "tags": [["t", topic]], "content": content,
        });
        let id = event_id(&ev);
        let id_bytes = hex::decode(&id).unwrap_or_default();
        let mut aux = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut aux);
        let sig = match self.sk.sign_raw(&id_bytes, &aux) {
            Ok(s) => hex::encode(s.to_bytes()),
            Err(_) => return PublishResult { ok: 0, sent: 0, relays: 0, brokers: 0 },
        };
        ev["id"] = json!(id);
        ev["sig"] = json!(sig);
        self.seen.lock().unwrap().insert(id.clone()); // ignore our own echo

        let ev_text = ev.to_string();
        let mut brokers = 0;
        for ep in self.endpoints.iter().filter(|e| e.kind == "mqtt" && e.open.load(Ordering::Relaxed)) {
            Self::send_to(ep, Message::binary(mqtt_publish(&format!("{MQTT_PREFIX}{topic}"), ev_text.as_bytes())));
            brokers += 1;
        }
        let open: Vec<_> = self.endpoints.iter().filter(|e| e.kind == "nostr" && e.open.load(Ordering::Relaxed)).collect();
        let (tx, mut rx) = unbounded_channel::<bool>();
        self.acks.lock().unwrap().insert(id.clone(), tx);
        let event_msg = json!(["EVENT", ev]).to_string();
        for ep in &open {
            Self::send_to(ep, Message::text(event_msg.clone()));
        }
        let mut relays = 0;
        let mut pending = open.len();
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        while pending > 0 {
            tokio::select! {
                Some(ok) = rx.recv() => { if ok { relays += 1; } pending -= 1; }
                _ = &mut deadline => break,
            }
        }
        self.acks.lock().unwrap().remove(&id);
        PublishResult { ok: relays + brokers, sent: open.len() + brokers, relays, brokers }
    }

    fn on_event(&self, ev: &Value) {
        let (Some(id), Some(content), Some(kind)) = (ev["id"].as_str(), ev["content"].as_str(), ev["kind"].as_u64()) else { return };
        if kind != KIND || content.len() > 64 * 1024 || self.seen.lock().unwrap().contains(id) {
            return;
        }
        let topic = ev["tags"].as_array().and_then(|tags| {
            tags.iter().find(|t| t[0] == "t").and_then(|t| t[1].as_str().map(|s| s.to_string()))
        });
        let Some(topic) = topic else { return };
        if !self.subs.lock().unwrap().contains_key(&topic) {
            return;
        }
        // Never trust the transport: check the hash and the author's signature ourselves.
        if event_id(ev) != id {
            return;
        }
        let ok = (|| -> Option<bool> {
            let pk = VerifyingKey::from_bytes(&hex::decode(ev["pubkey"].as_str()?).ok()?).ok()?;
            let sig = Signature::try_from(hex::decode(ev["sig"].as_str()?).ok()?.as_slice()).ok()?;
            Some(pk.verify_raw(&hex::decode(id).ok()?, &sig).is_ok())
        })()
        .unwrap_or(false);
        if !ok {
            return;
        }
        let created = ev["created_at"].as_i64().unwrap_or(0);
        if (self.now() / 1000 - created).abs() > CLOCK_SKEW_S {
            return;
        }
        {
            let mut seen = self.seen.lock().unwrap();
            seen.insert(id.to_string());
            if seen.len() > 5000 {
                seen.clear();
            }
        }
        (self.emit)(
            "rv:message",
            serde_json::to_value(Incoming { topic, content: content.to_string(), pubkey: ev["pubkey"].as_str().unwrap_or("").to_string() }).unwrap_or_default(),
        );
    }

    // ---- Nostr relay connection (reconnects with backoff) ----
    async fn run_relay(self: Arc<Self>, ep: Arc<Endpoint>) {
        let mut backoff = 1u64;
        loop {
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(ep.url.as_str()).await {
                backoff = 1;
                let (mut sink, mut stream) = ws.split();
                let (tx, mut rx) = unbounded_channel::<Message>();
                *ep.tx.lock().unwrap() = Some(tx);
                ep.open.store(true, Ordering::Relaxed);
                let subs: Vec<(String, Sub)> = self.subs.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (topic, sub) in &subs {
                    let _ = sink.send(Message::text(self.req_json(topic, sub))).await;
                }
                self.emit_status();
                let mut eose: HashSet<String> = HashSet::new();
                loop {
                    tokio::select! {
                        out = rx.recv() => match out {
                            Some(m) => { if sink.send(m).await.is_err() { break; } }
                            None => break,
                        },
                        inc = stream.next() => match inc {
                            Some(Ok(Message::Text(t))) => self.on_relay_text(&t, &mut eose),
                            Some(Ok(Message::Ping(p))) => { let _ = sink.send(Message::Pong(p)).await; }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                    }
                }
                ep.open.store(false, Ordering::Relaxed);
                *ep.tx.lock().unwrap() = None;
                self.emit_status();
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }

    fn on_relay_text(&self, text: &str, eose: &mut HashSet<String>) {
        let Ok(msg) = serde_json::from_str::<Value>(text) else { return };
        match msg[0].as_str() {
            Some("EVENT") => {
                let sub_id = msg[1].as_str().unwrap_or("");
                let live_only = self.subs.lock().unwrap().values().any(|s| s.id == sub_id && s.live_only);
                // Stored events (before EOSE) on a live-only topic are history: skip them.
                if live_only && !eose.contains(sub_id) {
                    return;
                }
                self.on_event(&msg[2]);
            }
            Some("EOSE") => {
                if let Some(s) = msg[1].as_str() {
                    eose.insert(s.to_string());
                }
            }
            Some("OK") => {
                if let (Some(id), Some(ok)) = (msg[1].as_str(), msg[2].as_bool()) {
                    if let Some(tx) = self.acks.lock().unwrap().get(id) {
                        let _ = tx.send(ok);
                    }
                }
            }
            _ => {}
        }
    }

    // ---- MQTT 3.1.1 over WebSocket, QoS 0 ----
    async fn run_broker(self: Arc<Self>, ep: Arc<Endpoint>) {
        let mut backoff = 1u64;
        loop {
            let mut req = match ep.url.as_str().into_client_request() {
                Ok(r) => r,
                Err(_) => return,
            };
            req.headers_mut().insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(req).await {
                let (mut sink, mut stream) = ws.split();
                let _ = sink.send(Message::binary(mqtt_connect(&format!("md{}", rand_hex(8))))).await;
                let (tx, mut rx) = unbounded_channel::<Message>();
                let mut buf: Vec<u8> = Vec::new();
                let mut ping = tokio::time::interval(Duration::from_secs(45));
                loop {
                    tokio::select! {
                        out = rx.recv() => match out {
                            Some(m) => { if sink.send(m).await.is_err() { break; } }
                            None => break,
                        },
                        _ = ping.tick() => { if ep.open.load(Ordering::Relaxed) { let _ = sink.send(Message::binary(vec![0xc0, 0])).await; } }
                        inc = stream.next() => match inc {
                            Some(Ok(Message::Binary(b))) => {
                                buf.extend_from_slice(&b);
                                while let Some((ptype, body)) = mqtt_take_packet(&mut buf) {
                                    match ptype >> 4 {
                                        2 => {
                                            // CONNACK
                                            if body.get(1) != Some(&0) { break; }
                                            *ep.tx.lock().unwrap() = Some(tx.clone());
                                            ep.open.store(true, Ordering::Relaxed);
                                            backoff = 1;
                                            let topics: Vec<String> = self.subs.lock().unwrap().keys().cloned().collect();
                                            for t in topics { let _ = sink.send(Message::binary(mqtt_subscribe(&format!("{MQTT_PREFIX}{t}")))).await; }
                                            self.emit_status();
                                        }
                                        3 => {
                                            // PUBLISH
                                            if let Some((topic, payload)) = mqtt_parse_publish(ptype, &body) {
                                                if topic.starts_with(MQTT_PREFIX) && payload.len() <= 64 * 1024 {
                                                    if let Ok(ev) = serde_json::from_slice::<Value>(&payload) { self.on_event(&ev); }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(p))) => { let _ = sink.send(Message::Pong(p)).await; }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                    }
                }
                ep.open.store(false, Ordering::Relaxed);
                *ep.tx.lock().unwrap() = None;
                self.emit_status();
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }
}

// ---- MQTT packet helpers ----
fn mqtt_len(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return out;
        }
    }
}

fn mqtt_str(s: &str) -> Vec<u8> {
    let mut v = (s.len() as u16).to_be_bytes().to_vec();
    v.extend_from_slice(s.as_bytes());
    v
}

fn mqtt_packet(t: u8, body: Vec<u8>) -> Vec<u8> {
    let mut v = vec![t];
    v.extend(mqtt_len(body.len()));
    v.extend(body);
    v
}

fn mqtt_connect(client_id: &str) -> Vec<u8> {
    let mut body = mqtt_str("MQTT");
    body.extend_from_slice(&[4, 0x02, 0, 60]); // v3.1.1, clean session, 60 s keep-alive
    body.extend(mqtt_str(client_id));
    mqtt_packet(0x10, body)
}

fn packet_id() -> [u8; 2] {
    let mut b = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut b);
    if b == [0, 0] {
        b[1] = 1;
    }
    b
}

fn mqtt_subscribe(topic: &str) -> Vec<u8> {
    let mut body = packet_id().to_vec();
    body.extend(mqtt_str(topic));
    body.push(0);
    mqtt_packet(0x82, body)
}

fn mqtt_unsubscribe(topic: &str) -> Vec<u8> {
    let mut body = packet_id().to_vec();
    body.extend(mqtt_str(topic));
    mqtt_packet(0xa2, body)
}

fn mqtt_publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = mqtt_str(topic);
    body.extend_from_slice(payload);
    mqtt_packet(0x30, body)
}

// WebSocket frames may carry partial or several MQTT packets.
fn mqtt_take_packet(buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    if buf.len() < 2 {
        return None;
    }
    let (mut len, mut mult, mut i) = (0usize, 1usize, 1usize);
    loop {
        let b = *buf.get(i)?;
        len += (b & 0x7f) as usize * mult;
        mult *= 128;
        if b & 0x80 == 0 {
            break;
        }
        i += 1;
        if i > 4 {
            buf.clear();
            return None;
        }
    }
    let start = i + 1;
    if buf.len() < start + len {
        return None;
    }
    let ptype = buf[0];
    let body = buf[start..start + len].to_vec();
    buf.drain(..start + len);
    Some((ptype, body))
}

fn mqtt_parse_publish(ptype: u8, body: &[u8]) -> Option<(String, Vec<u8>)> {
    let qos = (ptype >> 1) & 3;
    let tlen = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize;
    let topic = String::from_utf8(body.get(2..2 + tlen)?.to_vec()).ok()?;
    let start = 2 + tlen + if qos > 0 { 2 } else { 0 };
    Some((topic, body.get(start..)?.to_vec()))
}

// Relays reject events whose timestamp is far from their clock, and a computer's clock
// can be off by many minutes: take time from the network (Cloudflare trace, else Date).
async fn sync_clock() -> Option<i64> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().ok()?;
    let t0 = now_ms();
    if let Ok(res) = client.get("https://cloudflare.com/cdn-cgi/trace").send().await {
        if let Ok(text) = res.text().await {
            if let Some(ts) = text.lines().find_map(|l| l.strip_prefix("ts=")).and_then(|v| v.parse::<f64>().ok()) {
                return Some((ts * 1000.0) as i64 - (t0 + now_ms()) / 2);
            }
        }
    }
    for url in ["https://www.google.com", "https://www.microsoft.com"] {
        let t0 = now_ms();
        if let Ok(res) = client.head(url).send().await {
            if let Some(date) = res.headers().get("date").and_then(|d| d.to_str().ok()).and_then(parse_http_date) {
                return Some(date + 500 - (t0 + now_ms()) / 2);
            }
        }
    }
    None
}

// "Tue, 15 Oct 2026 12:34:56 GMT" -> unix ms
fn parse_http_date(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    let day: i64 = p[1].parse().ok()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == p[2])? as i64 + 1;
    let year: i64 = p[3].parse().ok()?;
    let hms: Vec<i64> = p[4].split(':').filter_map(|x| x.parse().ok()).collect();
    if hms.len() != 3 {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hms[0] * 3600 + hms[1] * 60 + hms[2]) * 1000)
}
