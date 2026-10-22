// Everything between the network and the UI: the connect-by-ID handshake (both roles),
// sessions, the request queue, presence, chat, files and clipboard. Runs on the UI thread
// (cheap work only); network and media live on the runtime and decoder threads.
// Wire-compatible with the web builds' app.js.
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::audio::{Mic, Player};
use crate::bus::{self, Msg};
use crate::fm::{self, FileManager, Pane, Transfer};

mod actions;
mod clip;
mod tunnel;
pub use actions::{DrawStroke, Reconnect};
pub use tunnel::Tunnel;
use crate::i18n::{t, tf};
use crate::input::Input;
use crate::mdp::{Media, Peer, Session};
use crate::media::capture::Screen;
use crate::store::{LogEntry, Permissions};
use crate::platform::perms::{self, Perms};
use crate::quality::{Mode, Quality};
use crate::rendezvous::Rendezvous;
use crate::rv::{self, Key};
use crate::store::{self, Store};
use crate::video::Feed;

pub const MAX_HOST: usize = 5;
const REQUEST_SECS: u64 = 60;
const PRESENCE_EVERY: Duration = Duration::from_secs(45);
const PRESENCE_FRESH: Duration = Duration::from_secs(100);
pub const VIEWER_COLORS: [[u8; 3]; 5] = [[230, 57, 70], [42, 157, 143], [69, 123, 157], [244, 162, 97], [131, 56, 236]];

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

pub fn os_name(p: &str) -> &'static str {
    match p {
        "darwin" => "macOS",
        "win32" => "Windows",
        "linux" => "Linux",
        _ => "",
    }
}

pub fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().trim_end_matches(".local").chars().take(60).collect()
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
    pub path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct ChatLine {
    pub mine: bool,
    pub text: String,
    /// local time, "HH:MM"
    pub time: String,
}

fn hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

#[derive(Default)]
pub struct Chat {
    pub lines: Vec<ChatLine>,
    pub open: bool,
    pub unread: usize,
    pub input: String,
}

impl Chat {
    fn receive(&mut self, text: &str) {
        self.lines.push(ChatLine { mine: false, text: text.chars().take(4000).collect(), time: hhmm() });
        if !self.open {
            self.unread += 1;
        }
    }
}

/// An incoming file being written to Downloads.
struct Incoming {
    id: String,
    name: String,
    file: std::fs::File,
    path: PathBuf,
    /// a file manager batch (no toast per file)
    batch: Option<String>,
}

// ---------------------------------------------------------------- viewer tabs ---

#[derive(PartialEq, Clone)]
pub enum TabState {
    Requesting,
    Waiting,
    Accepted,
    Live,
    Failed(String),
}

struct ViewerHs {
    topic: String,
    key: Key,
    host_peer: Option<String>,
    deadline: Instant,
}

pub struct Tab {
    pub key: u32,
    pub peer_id: String,
    pub label: String,
    pub os: String,
    pub state: TabState,
    pub sas: String,
    hs: Option<ViewerHs>,
    /// the request, until a rendezvous channel is up to carry it
    outbox: Option<Value>,
    pub sid: Option<u32>,
    pub feed: Option<Arc<Feed>>,
    /// what the host lets us do
    pub remote: Permissions,
    /// the host asks for the unattended-access password
    pub auth: Option<AuthAsk>,
    live_since: Option<Instant>,
    pub remote_mic: bool,
    pub player: Option<Arc<Player>>,
    pub mic: Option<Mic>,
    pub mic_error: bool,
    pub chat: Chat,
    pub quality: Mode,
    pub stats: String,
    stats_prev: Option<(u64, Instant)>,
    pub sent_view: (u32, u32),
    incoming: Option<Incoming>,
    pub texture: Option<egui::TextureHandle>,
    thumb_saved: bool,
    /// the host's displays and the one shown (switchable from the toolbar)
    pub screens: Vec<RemoteScreen>,
    pub screen: Option<u32>,
    /// how the remote picture sits in the stage, and the scroll offset (points) in 1:1
    pub view: ViewMode,
    pub pan: egui::Vec2,
    /// the file manager (both disks side by side)
    pub fm: FileManager,
    /// the host's screen is blacked out for the people there
    pub privacy: bool,
    pub lock_on_end: bool,
    /// the remote computer restarts: reconnect when it is back
    pub reconnect: Option<Reconnect>,
    /// keyboard: send characters (layout of this computer) instead of physical keys
    pub kbd_text: bool,
    /// why no picture arrives (from the host), shown on the connecting card
    pub no_picture: Option<String>,
    /// the host's pointer when it is sent apart from the picture: where (0..1 on the
    /// display; None while it is elsewhere) and its look (`shape_gen` counts changes)
    pub pointer: Option<(f32, f32)>,
    pub pointer_shape: Option<RemoteCursor>,
    pub pointer_gen: u32,
}

/// The host's pointer image; size and hot spot in the host display's pixels.
pub struct RemoteCursor {
    pub image: egui::ColorImage,
    pub size: egui::Vec2,
    pub hot: egui::Vec2,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ViewMode {
    /// the whole screen, aspect kept (letterboxed)
    Fit,
    /// one remote pixel per local pixel, scrolled
    Actual,
    /// fills the stage, aspect ignored
    Stretch,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Fit => "fit",
            ViewMode::Actual => "actual",
            ViewMode::Stretch => "stretch",
        }
    }
}

/// Viewer: the host's password prompt (unattended access).
#[derive(Default)]
pub struct AuthAsk {
    pub nonce: String,
    pub salt: String,
    pub totp: bool,
    pub password: String,
    pub code: String,
    pub remember: bool,
    pub error: String,
    /// a proof is on its way (deriving the key takes a moment)
    pub busy: bool,
    /// the remembered key was tried already
    tried_saved: bool,
}

/// Host: a session that waits for the unattended-access password.
pub struct AuthWait {
    nonce: String,
    tries: u8,
    since: Instant,
}

/// One of the host's displays, as the viewer knows it.
#[derive(Clone)]
pub struct RemoteScreen {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

// ------------------------------------------------------------------ host side ---

#[derive(Clone)]
pub struct Request {
    reply: String,
    key: Key,
    peer: String,
    pub name: String,
    pub os: String,
    pub from: String,
    native: bool,
    /// unattended access: the session already started for this request (waiting for
    /// either the password or the Accept button)
    host_key: Option<u32>,
    pub at: Instant,
}

#[derive(PartialEq)]
pub enum HostState {
    Starting,
    Offered,
    Live,
}

pub struct Host {
    pub key: u32,
    pub req: Request,
    pub perms: Permissions,
    /// Some while the viewer has not proven the password yet (unattended access)
    pub auth: Option<AuthWait>,
    pub unattended: bool,
    live_since: Option<Instant>,
    pub color: [u8; 3],
    hs_topic: Option<String>,
    hs_deadline: Instant,
    pub sid: Option<u32>,
    pub state: HostState,
    pub sas: String,
    pub chat: Chat,
    quality: Quality,
    src: (u32, u32),
    last_tick: Instant,
    pub stats: String,
    stats_prev: Option<(u64, Instant)>,
    incoming: Option<Incoming>,
    pub remote_mic: bool,
    pub player: Option<Arc<Player>>,
    pub mic: Option<Mic>,
    /// where this viewer last pointed on the shared screen (for the host overlay)
    pub pointer: Option<Pointer>,
    /// the display this viewer is shown
    pub screen: Option<Screen>,
    /// downloads the viewer is taking from the file manager, by batch (set to stop one)
    fm_cancel: HashMap<String, Arc<AtomicBool>>,
    pub privacy: bool,
    lock_on_end: bool,
    /// the viewer's whiteboard strokes on this screen
    pub strokes: Vec<DrawStroke>,
    /// "no picture" was reported for this session (capture never produced a frame)
    no_picture_sent: bool,
}

/// A viewer's pointer on the host's screen, in monitor points.
#[derive(Clone, Copy)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    /// "move", "down", "scroll" or "key": what the label says the viewer is doing
    pub kind: &'static str,
    pub at: Instant,
    pub click: Option<Instant>,
}

// ------------------------------------------------------------------------ core ---

type Registry = Arc<Mutex<HashMap<u32, Arc<Session>>>>;

pub struct Core {
    pub store: Store,
    pub rv: Arc<Rendezvous>,
    pub rv_status: (usize, usize),
    my_topic: String,
    pub name: String,
    sessions: Registry,
    next: u32,
    pub tabs: Vec<Tab>,
    pub active: Option<u32>,
    pub hosts: Vec<Host>,
    pub pending: Option<Request>,
    pub queue: VecDeque<Request>,
    presence: HashMap<String, (Instant, bool)>,
    presence_topic: String,
    presence_key: Key,
    presence_raw: String,
    last_poll: Option<Instant>,
    pong_log: HashMap<String, Instant>,
    pong_window: (Instant, u32),
    pub perms: Perms,
    perms_checked: Instant,
    pub toasts: Vec<Toast>,
    pub input: Input,
    pub screens: Vec<Screen>,
    pub screen: Option<u32>,
    clip_last: String,
    pub capture_tab: Option<u32>,
    /// privacy mode active on this computer (some viewer asked for it)
    pub privacy_on: bool,
    /// switch sides: our offer (token, tab, when), and the tab to close once reversed
    switch_offer: Option<(String, u32, Instant)>,
    switch_close_tab: Option<u32>,
    /// viewer: TCP tunnels into the sessions
    pub tunnels: Vec<Tunnel>,
    next_tunnel: u32,
    /// viewer: the key derived for a tab's password prompt, kept until the host says ok
    auth_keys: HashMap<u32, String>,
}

impl Core {
    pub fn new() -> Self {
        let store = Store::load();
        crate::i18n::set(&store.lang);
        let rv = Rendezvous::new(bus::emitter());
        rv.start();
        let my_topic = rv::topic_for_id(&store.my_id);
        rv.subscribe(&my_topic, true);
        let (presence_key, presence_raw) = rv::new_session_key();
        let presence_topic = rv::random_topic();
        rv.subscribe(&presence_topic, false);
        // The ID keys are slow to derive (PBKDF2): warm them up off the UI thread.
        let ids: Vec<String> = std::iter::once(store.my_id.clone()).chain(store.recent.iter().map(|r| r.id.clone())).collect();
        crate::rt::spawn_blocking(move || {
            for id in ids {
                rv::key_for_id(&id);
            }
        });
        clip::start_watch();
        let mut core = Core {
            store,
            rv,
            rv_status: (0, 0),
            my_topic,
            name: hostname(),
            sessions: Default::default(),
            next: 0,
            tabs: Vec::new(),
            active: None,
            hosts: Vec::new(),
            pending: None,
            queue: VecDeque::new(),
            presence: HashMap::new(),
            presence_topic,
            presence_key,
            presence_raw,
            last_poll: None,
            pong_log: HashMap::new(),
            pong_window: (Instant::now(), 0),
            perms: perms::status(),
            perms_checked: Instant::now(),
            toasts: Vec::new(),
            input: Input::new(),
            screens: Vec::new(),
            screen: None,
            clip_last: String::new(),
            capture_tab: None,
            privacy_on: false,
            switch_offer: None,
            switch_close_tab: None,
            tunnels: Vec::new(),
            next_tunnel: 0,
            auth_keys: HashMap::new(),
        };
        // Test hook: MANYDESK_NOTHUMBS=1 skips the display pictures (two instances on one Mac
        // would otherwise hold ScreenCaptureKit up for each other).
        core.refresh_screens(std::env::var_os("MANYDESK_NOTHUMBS").is_none());
        core
    }

    fn next_id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), error, until: Instant::now() + Duration::from_secs(if error { 12 } else { 5 }), path: None });
    }

    fn send(&self, topic: &str, key: &Key, mut obj: Value) {
        obj["ts"] = json!(rv::now_ms());
        let content = rv::seal(key, &obj);
        let (rv, topic) = (self.rv.clone(), topic.to_string());
        crate::rt::spawn(async move {
            rv.publish(&topic, &content).await;
        });
    }

    fn session(&self, sid: Option<u32>) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(&sid?).cloned()
    }

    fn ctl(&self, sid: Option<u32>, msg: Value) {
        if let Some(s) = self.session(sid) {
            let text = msg.to_string();
            crate::rt::spawn(async move {
                let _ = s.send_text(false, &text).await;
            });
        }
    }

    /// `thumbs`: also take a picture of every display (for the Share page's picker). Only
    /// when no session is starting: on macOS the capture goes through ScreenCaptureKit and
    /// holds it up — a stream started meanwhile misses its timeout and falls back to the
    /// slow software path.
    pub fn refresh_screens(&mut self, thumbs: bool) {
        // The list is cheap; the thumbnails come later from a thread.
        let old = std::mem::take(&mut self.screens);
        self.screens = crate::media::capture::screens(false);
        for s in &mut self.screens {
            s.thumb = old.iter().find(|o| o.id == s.id).map(|o| o.thumb.clone()).unwrap_or_default();
        }
        static BUSY: AtomicBool = AtomicBool::new(false);
        if thumbs && !BUSY.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let t0 = Instant::now();
                let list = crate::media::capture::screens(true);
                crate::perf::add("thumbs", t0.elapsed());
                if std::env::var_os("MANYDESK_PERF").is_some() {
                    eprintln!("[perf] screen thumbnails took {:?}", t0.elapsed());
                }
                BUSY.store(false, Ordering::SeqCst);
                bus::emit("screens:thumbs", json!(list));
            });
        }
        if self.screen.is_none_or(|id| !self.screens.iter().any(|s| s.id == id)) {
            self.screen = self.screens.first().map(|s| s.id);
        }
    }

    /// "online" | "busy" | "offline" | "checking" (right after a ping round).
    pub fn presence_of(&self, id: &str) -> &'static str {
        if let Some((at, busy)) = self.presence.get(id) {
            if at.elapsed() < PRESENCE_FRESH {
                return if *busy { "busy" } else { "online" };
            }
        }
        if self.last_poll.is_some_and(|t| t.elapsed() < Duration::from_secs(7)) {
            "checking"
        } else {
            "offline"
        }
    }

    pub fn tab(&mut self, key: u32) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.key == key)
    }

    // ---------------------------------------------------------- per frame ---

    pub fn tick(&mut self, rx: &std::sync::mpsc::Receiver<Msg>) {
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Event(name, v) => self.on_event(name, v),
                Msg::File(sid, data) => self.on_file_bytes(sid, data),
            }
        }
        let now = Instant::now();
        // Requests wait for the first rendezvous channel (e.g. right after start).
        if self.rv_status.0 > 0 {
            for i in 0..self.tabs.len() {
                if let Some(req) = self.tabs[i].outbox.take() {
                    let id = self.tabs[i].peer_id.clone();
                    self.send(&rv::topic_for_id(&id), &rv::key_for_id(&id), req);
                    self.tabs[i].state = TabState::Waiting;
                    if let Some(hs) = self.tabs[i].hs.as_mut() {
                        hs.deadline = now + Duration::from_secs(75);
                    }
                }
            }
        }
        self.tick_reconnect();
        self.update_clip_active();
        // Viewer handshakes that got no answer.
        for i in 0..self.tabs.len() {
            if self.tabs[i].hs.as_ref().is_some_and(|h| h.host_peer.is_none() && now > h.deadline) {
                self.end_handshake(i);
                self.tabs[i].state = TabState::Failed(t("req.timeout"));
            }
        }
        // Hosts whose offer was never answered.
        let stale: Vec<u32> = self.hosts.iter().filter(|h| h.state != HostState::Live && now > h.hs_deadline).map(|h| h.key).collect();
        for k in stale {
            let name = self.hosts.iter().find(|h| h.key == k).map(|h| h.req.name.clone()).unwrap_or_default();
            self.toast(format!("{name}: {}", t("err.noAnswer")), true);
            self.close_host(k, false);
        }
        // The request prompt times out (counts as Decline).
        if self.pending.as_ref().is_some_and(|r| r.at.elapsed() > Duration::from_secs(REQUEST_SECS)) {
            if self.pending.as_ref().is_some_and(|r| r.host_key.is_some()) {
                self.pending = None; // the session keeps waiting for the password
                self.show_next();
            } else {
                self.decide(false, Permissions::NONE);
            }
        }
        // Unattended sessions that never got the password.
        let unproven: Vec<u32> = self.hosts.iter().filter(|h| h.auth.as_ref().is_some_and(|a| a.since.elapsed() > Duration::from_secs(180))).map(|h| h.key).collect();
        for k in unproven {
            self.close_host(k, true);
        }
        if self.last_poll.is_none_or(|t| t.elapsed() > PRESENCE_EVERY) && self.rv_status.0 > 0 {
            self.poll_presence();
        }
        if self.perms_checked.elapsed() > Duration::from_secs(3) {
            self.perms = perms::status();
            self.perms_checked = now;
        }
        self.update_stats();
        self.toasts.retain(|t| t.until > now);
    }

    fn on_event(&mut self, name: &str, v: Value) {
        match name {
            "rv:status" => self.rv_status = (v["connected"].as_u64().unwrap_or(0) as usize, v["total"].as_u64().unwrap_or(0) as usize),
            "rv:message" => self.on_rv(v),
            "mdp" => {
                if std::env::var_os("MANYDESK_DEBUG").is_some() && v["ev"]["e"] != "ctl" {
                    eprintln!("[mdp] {v}");
                }
                self.on_mdp(v["sid"].as_u64().unwrap_or(0) as u32, &v["ev"])
            }
            "viewer:ready" => self.on_viewer_ready(v),
            "host:offered" => self.on_host_offered(v),
            "clip" => self.on_local_clipboard(v["text"].as_str().unwrap_or("")),
            "screens:thumbs" => {
                for t in serde_json::from_value::<Vec<Screen>>(v).unwrap_or_default() {
                    if let Some(s) = self.screens.iter_mut().find(|s| s.id == t.id) {
                        s.thumb = t.thumb;
                    }
                }
            }
            "viewer:authkey" => {
                if let (Some(tab), Some(k)) = (v["tab"].as_u64(), v["k"].as_str()) {
                    self.auth_keys.insert(tab as u32, k.to_string());
                }
            }
            "test:fm" => self.test_fm(v["tab"].as_u64().unwrap_or(0) as u32, v["step"].as_u64().unwrap_or(0)),
            "test:tun" => {
                let (key, spec) = (v["tab"].as_u64().unwrap_or(0) as u32, v["spec"].as_str().unwrap_or("").to_string());
                let (h, p) = spec.split_once(':').unwrap_or(("127.0.0.1", "0"));
                match self.tunnel_open(key, "17777", h, p) {
                    Ok(()) => eprintln!("[testtun] opened 127.0.0.1:17777 -> {spec}"),
                    Err(e) => eprintln!("[testtun] FAILED: {e}"),
                }
            }
            "test:screen" => {
                let key = v["tab"].as_u64().unwrap_or(0) as u32;
                if self.tabs.iter().any(|t| t.key == key && t.screens.len() > 1 && t.screen == t.screens.first().map(|s| s.id)) {
                    eprintln!("[test] switching display");
                    self.switch_screen(key, None, 1);
                }
            }
            "host:screen" => {
                let Some(i) = self.hosts.iter().position(|h| Some(h.key as u64) == v["host"].as_u64()) else { return };
                if let Ok(screen) = serde_json::from_value::<Screen>(v["screen"].clone()) {
                    self.hosts[i].src = (screen.width, screen.height);
                    self.hosts[i].pointer = None;
                    self.hosts[i].screen = Some(screen);
                    self.apply_quality(i);
                }
                self.send_screens(i);
            }
            "turn:cache" => {
                self.store.turn.cf_cache = serde_json::from_value(v).ok();
                self.store.save();
            }
            "fm:progress" | "fm:done" => self.on_fm_upload(name, &v),
            "privacy:check" => self.privacy_check(),
            "clip:files" | "clip:image" => self.on_local_clip(name, &v),
            "toast" => self.toast(v["text"].as_str().unwrap_or(""), v["error"].as_bool().unwrap_or(false)),
            _ => {}
        }
    }

    fn on_rv(&mut self, m: Value) {
        let topic = m["topic"].as_str().unwrap_or("").to_string();
        let content = m["content"].as_str().unwrap_or("");
        let pubkey = m["pubkey"].as_str().unwrap_or("").to_string();
        if std::env::var_os("MANYDESK_DEBUG").is_some() {
            let host_topics: Vec<_> = self.hosts.iter().map(|h| h.hs_topic.clone().unwrap_or_default()[..8.min(h.hs_topic.as_ref().map_or(0, |t| t.len()))].to_string()).collect();
            eprintln!("[rv] topic {} (mine {}, hosts {:?})", &topic[..8.min(topic.len())], topic == self.my_topic, host_topics);
        }
        if topic == self.my_topic {
            let key = rv::key_for_id(&self.store.my_id);
            if let Some(msg) = rv::open(&key, content) {
                self.on_my_topic(msg, pubkey);
            }
        } else if topic == self.presence_topic {
            if let Some(msg) = rv::open(&self.presence_key, content) {
                self.on_pong(msg);
            }
        } else if let Some(i) = self.hosts.iter().position(|h| h.hs_topic.as_deref() == Some(&topic)) {
            if pubkey != self.hosts[i].req.peer {
                return; // only the requester that was accepted may answer
            }
            if let Some(msg) = rv::open(&self.hosts[i].req.key.clone(), content) {
                self.on_answer(i, msg);
            }
        } else if let Some(i) = self.tabs.iter().position(|t| t.hs.as_ref().is_some_and(|h| h.topic == topic)) {
            let hs = self.tabs[i].hs.as_ref().unwrap();
            if hs.host_peer.as_ref().is_some_and(|p| *p != pubkey) {
                return;
            }
            if let Some(msg) = rv::open(&hs.key.clone(), content) {
                self.on_reply(i, msg, pubkey);
            }
        }
    }

    // ------------------------------------------------- viewer: connect by ID ---

    pub fn connect(&mut self, id: &str) {
        let id = rv::normalize_id(id);
        if id.len() != 9 {
            return self.toast(t("err.badId"), true);
        }
        if id == self.store.my_id {
            return self.toast(t("err.selfId"), true);
        }
        if let Some(t) = self.tabs.iter().find(|t| t.peer_id == id && !matches!(t.state, TabState::Failed(_))) {
            self.active = Some(t.key);
            return;
        }
        let key = self.next_id();
        let label = self.store.recent.iter().find(|r| r.id == id).map(|r| r.name.clone()).unwrap_or_else(|| rv::format_id(&id));
        let (session_key, raw) = rv::new_session_key();
        let topic = rv::random_topic();
        self.rv.subscribe(&topic, false);
        let req = json!({
            "t": "req", "reply": topic, "k": raw, "name": self.name, "os": platform(), "from": self.store.my_id, "caps": ["mdp1"],
        });
        self.tabs.push(Tab {
            outbox: Some(req),
            key,
            peer_id: id,
            label,
            os: String::new(),
            state: TabState::Requesting,
            sas: String::new(),
            hs: Some(ViewerHs { topic, key: session_key, host_peer: None, deadline: Instant::now() + Duration::from_secs(75) }),
            sid: None,
            feed: None,
            remote: Permissions::NONE,
            auth: None,
            live_since: None,
            remote_mic: false,
            player: None,
            mic: None,
            mic_error: false,
            chat: Chat::default(),
            quality: Mode::Auto,
            stats: String::new(),
            stats_prev: None,
            sent_view: (0, 0),
            incoming: None,
            texture: None,
            screens: Vec::new(),
            screen: None,
            thumb_saved: false,
            // Test hook: MANYDESK_VIEW=actual starts in 1:1 (unscaled picture).
            view: if std::env::var("MANYDESK_VIEW").as_deref() == Ok("actual") { ViewMode::Actual } else { ViewMode::Fit },
            pan: egui::Vec2::ZERO,
            fm: FileManager::default(),
            privacy: false,
            lock_on_end: false,
            reconnect: None,
            kbd_text: false,
            no_picture: None,
            pointer: None,
            pointer_shape: None,
            pointer_gen: 0,
        });
        self.active = Some(key);
    }

    fn end_handshake(&mut self, i: usize) {
        if let Some(hs) = self.tabs[i].hs.take() {
            self.rv.unsubscribe(&hs.topic);
        }
    }

    fn on_reply(&mut self, i: usize, msg: Value, pubkey: String) {
        match msg["t"].as_str() {
            Some("reject") => {
                self.end_handshake(i);
                let reason = match msg["reason"].as_str() {
                    Some("acl") => format!(" ({})", t("err.acl")),
                    Some(r) => format!(" ({})", r.chars().take(120).collect::<String>()),
                    None => String::new(),
                };
                self.tabs[i].state = TabState::Failed(t("req.rejected") + &reason);
            }
            Some("busy") => {
                self.end_handshake(i);
                self.tabs[i].state = TabState::Failed(t("req.busy"));
            }
            Some("offer") if self.tabs[i].hs.as_ref().is_some_and(|h| h.host_peer.is_none()) => {
                let reply = msg["reply"].as_str().unwrap_or("").to_string();
                let code = msg["code"].as_str().unwrap_or("");
                if !is_hex64(&reply) {
                    return;
                }
                let tab = &mut self.tabs[i];
                if let Some(hs) = tab.hs.as_mut() {
                    hs.host_peer = Some(pubkey);
                }
                let name: String = msg["name"].as_str().unwrap_or("").chars().take(60).collect();
                if !name.is_empty() {
                    tab.label = name;
                }
                tab.os = msg["os"].as_str().unwrap_or("").to_string();
                let Some(offer) = rv::decode_code(code).filter(|o| o.get("mdp").is_some()) else {
                    // An older version on the other side (WebRTC only).
                    tab.state = TabState::Failed(t("err.oldPeer"));
                    self.end_handshake(i);
                    return;
                };
                let Ok(peer) = serde_json::from_value::<Peer>(offer["mdp"].clone()) else { return };
                tab.state = TabState::Accepted;
                let hs_key = tab.hs.as_ref().unwrap().key;
                self.start_viewer(i, peer, reply, hs_key);
            }
            _ => {}
        }
    }

    fn start_viewer(&mut self, i: usize, peer: Peer, reply: String, hs_key: Key) {
        let sid = self.next_id();
        let tab_key = self.tabs[i].key;
        let reg = self.sessions.clone();
        let reg2 = reg.clone();
        let feed = Arc::new(Feed::start(move || {
            if let Some(s) = reg2.lock().unwrap().get(&sid) {
                s.request_keyframe();
            }
        }));
        self.tabs[i].feed = Some(feed.clone());
        self.tabs[i].sid = Some(sid);
        let player = Arc::new(Player::start());
        self.tabs[i].player = Some(player.clone());
        let stun = self.store.stun.clone();
        let turn_cfg = self.store.turn.clone();
        let rv = self.rv.clone();
        crate::rt::spawn(async move {
            let turn = relay_server(turn_cfg).await;
            let events = Arc::new(move |ev: Value| bus::emit("mdp", json!({ "sid": sid, "ev": ev })));
            let f = feed.clone();
            let media = Arc::new(move |m: Media| match m {
                Media::Video { key, data, .. } => {
                    f.push(key, data);
                }
                Media::File(d) => bus::file(sid, d),
                Media::Audio(p) => player.push(p),
            });
            let (s, prep) = match Session::prepare(false, stun, turn, events, media).await {
                Ok(x) => x,
                Err(e) => return bus::emit("viewer:ready", json!({ "tab": tab_key, "error": e })),
            };
            reg.lock().unwrap().insert(sid, s.clone());
            let sas = rv::security_code(&peer.fp, s.fingerprint());
            s.connect(peer);
            let answer = rv::encode_code(&json!({ "v": 1, "t": "answer", "mdp": { "cands": prep["cands"], "fp": prep["fp"], "token": "" } }));
            let mut obj = json!({ "t": "answer", "code": answer, "ts": rv::now_ms() });
            obj["ts"] = json!(rv::now_ms());
            rv.publish(&reply, &rv::seal(&hs_key, &obj)).await;
            bus::emit("viewer:ready", json!({ "tab": tab_key, "sas": sas }));
        });
    }

    fn on_viewer_ready(&mut self, v: Value) {
        let key = v["tab"].as_u64().unwrap_or(0) as u32;
        let Some(i) = self.tabs.iter().position(|t| t.key == key) else { return };
        if let Some(e) = v["error"].as_str() {
            self.tabs[i].state = TabState::Failed(tf("conn.failed", &[("e", e)]));
            return;
        }
        self.tabs[i].sas = v["sas"].as_str().unwrap_or("").to_string();
        // Keep the handshake topic: the host may still reject or time out.
        if let Some(hs) = self.tabs[i].hs.as_mut() {
            hs.deadline = Instant::now() + Duration::from_secs(45);
        }
    }

    pub fn close_tab(&mut self, key: u32) {
        let Some(i) = self.tabs.iter().position(|t| t.key == key) else { return };
        let tab = &self.tabs[i];
        if let Some(hs) = &tab.hs {
            if hs.host_peer.is_none() {
                self.send(&rv::topic_for_id(&tab.peer_id), &rv::key_for_id(&tab.peer_id), json!({ "t": "cancel", "reply": hs.topic }));
            }
        }
        self.end_handshake(i);
        self.log_tab(i);
        let tab = self.tabs.remove(i);
        self.tunnels.retain(|t| t.tab != key);
        for c in tab.fm.cancels.values() {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(f) = &tab.feed {
            f.stop();
        }
        if let Some(s) = self.sessions.lock().unwrap().remove(&tab.sid.unwrap_or(0)) {
            let text = json!({ "t": "bye" }).to_string();
            crate::rt::spawn(async move {
                let _ = s.send_text(false, &text).await;
                s.close();
            });
        }
        if self.capture_tab == Some(key) {
            self.set_capture(None);
        }
        if self.active == Some(key) {
            self.active = self.tabs.last().map(|t| t.key);
        }
    }

    /// The viewer stage size in device pixels: the host encodes no more than that.
    pub fn send_view(&mut self, key: u32, w: u32, h: u32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.key == key) else { return };
        if tab.state != TabState::Live || w == 0 || h == 0 {
            return;
        }
        let (pw, ph) = tab.sent_view;
        if pw.abs_diff(w) < 16 && ph.abs_diff(h) < 16 {
            return;
        }
        tab.sent_view = (w, h);
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "view", "w": w, "h": h }));
    }

    /// Viewer: a finished session goes into the connection log (once).
    fn log_tab(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        if let Some(t0) = tab.live_since.take() {
            let e = LogEntry { ts: rv::now_ms(), dir: "out".into(), id: tab.peer_id.clone(), name: tab.label.clone(), secs: t0.elapsed().as_secs(), unattended: false };
            self.store.log(e);
        }
    }

    /// Viewer: a remembered key for this computer answers the password prompt by itself
    /// (with TOTP only the code is asked for).
    fn try_saved_auth(&mut self, key: u32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.key == key) else { return };
        let saved = self.store.recent.iter().find(|r| r.id == tab.peer_id).and_then(|r| r.auth.clone());
        let Some(a) = tab.auth.as_mut() else { return };
        if let Some((salt, k)) = saved.filter(|(salt, _)| *salt == a.salt && !a.tried_saved) {
            a.tried_saved = true;
            let _ = salt;
            self.auth_keys.insert(key, k);
            if !a.totp {
                self.send_auth(key);
            }
        }
    }

    /// Viewer: answers the password prompt (the key is derived off the UI thread).
    pub fn send_auth(&mut self, key: u32) {
        let Some(s) = self.session(self.tabs.iter().find(|t| t.key == key).and_then(|t| t.sid)) else { return };
        let Some(tab) = self.tabs.iter_mut().find(|t| t.key == key) else { return };
        let Some(a) = tab.auth.as_mut() else { return };
        let code = if a.totp {
            match a.code.trim().parse::<u32>() {
                Ok(c) if a.code.trim().len() == 6 => Some(c),
                _ => {
                    a.error = t("auth.needCode");
                    return;
                }
            }
        } else {
            None
        };
        a.busy = true;
        a.error.clear();
        let (nonce, salt, password) = (a.nonce.clone(), a.salt.clone(), std::mem::take(&mut a.password));
        let saved = if password.is_empty() { self.auth_keys.get(&key).cloned() } else { None };
        crate::rt::spawn(async move {
            let k = match saved {
                Some(k) => k,
                None => hex::encode(crate::rt::spawn_blocking(move || crate::auth::derive(&password, &salt)).await.unwrap_or_default()),
            };
            let proof = crate::auth::proof(&hex::decode(&k).unwrap_or_default(), &nonce, &s.peer_fingerprint(), s.fingerprint(), code);
            let _ = s.send_text(false, &json!({ "t": "auth", "proof": proof }).to_string()).await;
            bus::emit("viewer:authkey", json!({ "tab": key, "k": k }));
        });
    }

    /// Viewer: show another of the host's displays (`step` ±1 cycles, as Ctrl+Alt+Shift+←/→).
    pub fn switch_screen(&mut self, key: u32, id: Option<u32>, step: i32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.key == key) else { return };
        if tab.screens.len() < 2 {
            return;
        }
        let id = id.unwrap_or_else(|| {
            let n = tab.screens.len() as i32;
            let cur = tab.screens.iter().position(|s| Some(s.id) == tab.screen).unwrap_or(0) as i32;
            tab.screens[((cur + step).rem_euclid(n)) as usize].id
        });
        if tab.screen == Some(id) {
            return;
        }
        tab.screen = Some(id);
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "screen", "id": id }));
    }

    pub fn set_quality(&mut self, key: u32, mode: Mode) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.key == key) else { return };
        tab.quality = mode;
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "quality", "mode": mode.as_str() }));
    }

    /// Viewer input (only for the tab that has the capture).
    pub fn input(&mut self, key: u32, msg: Value) {
        let Some(tab) = self.tabs.iter().find(|t| t.key == key) else { return };
        if tab.state == TabState::Live && tab.remote.control && tab.auth.is_none() {
            self.ctl(tab.sid, msg);
        }
    }

    pub fn set_capture(&mut self, key: Option<u32>) {
        if let (Some(old), true) = (self.capture_tab, key != self.capture_tab) {
            if let Some(t) = self.tabs.iter().find(|t| t.key == old) {
                self.ctl(t.sid, json!({ "t": "release" }));
            }
        }
        self.capture_tab = key;
    }

    pub fn send_chat(&mut self, viewer: Option<u32>, host: Option<u32>) {
        let (sid, text) = if let Some(k) = viewer {
            let Some(t) = self.tabs.iter_mut().find(|t| t.key == k) else { return };
            let text = std::mem::take(&mut t.chat.input).trim().to_string();
            if text.is_empty() {
                return;
            }
            t.chat.lines.push(ChatLine { mine: true, text: text.clone(), time: hhmm() });
            (t.sid, text)
        } else if let Some(k) = host {
            let Some(h) = self.hosts.iter_mut().find(|h| h.key == k) else { return };
            let text = std::mem::take(&mut h.chat.input).trim().to_string();
            if text.is_empty() {
                return;
            }
            h.chat.lines.push(ChatLine { mine: true, text: text.clone(), time: hhmm() });
            (h.sid, text)
        } else {
            return;
        };
        self.ctl(sid, json!({ "t": "chat", "text": text, "ts": rv::now_ms() }));
    }

    /// Microphone on/off for a viewer tab or a host session.
    pub fn set_mic(&mut self, viewer: Option<u32>, host: Option<u32>, on: bool) {
        let sid = if let Some(k) = viewer {
            self.tabs.iter().find(|t| t.key == k).and_then(|t| t.sid)
        } else {
            self.hosts.iter().find(|h| Some(h.key) == host).and_then(|h| h.sid)
        };
        let mic = if on {
            let reg = self.sessions.clone();
            match Mic::start(move |pkt| {
                if let Some(s) = sid.and_then(|id| reg.lock().unwrap().get(&id).cloned()) {
                    s.send_audio(&pkt);
                }
            }) {
                Ok(m) => Some(m),
                Err(e) => {
                    self.toast(tf("mic.failed", &[("e", &e)]), true);
                    None
                }
            }
        } else {
            None
        };
        let live = mic.is_some();
        if let Some(t) = viewer.and_then(|k| self.tabs.iter_mut().find(|t| t.key == k)) {
            t.mic = mic;
        } else if let Some(h) = host.and_then(|k| self.hosts.iter_mut().find(|h| h.key == k)) {
            h.mic = mic;
        }
        self.ctl(sid, json!({ "t": "mic", "on": live }));
    }

    /// Background tabs: remote audio muted and microphone paused, so a conversation
    /// only ever happens with the session on screen.
    pub fn apply_isolation(&mut self) {
        let active = self.active;
        let mut notify = Vec::new();
        for t in &mut self.tabs {
            let visible = active == Some(t.key);
            if let Some(p) = &t.player {
                p.muted.store(!visible, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(m) = &t.mic {
                if m.enabled.swap(visible, std::sync::atomic::Ordering::Relaxed) != visible {
                    notify.push((t.sid, visible));
                }
            }
        }
        for (sid, on) in notify {
            self.ctl(sid, json!({ "t": "mic", "on": on }));
        }
    }

    // ------------------------------------------------- host: requests ---

    fn on_my_topic(&mut self, msg: Value, pubkey: String) {
        match msg["t"].as_str() {
            Some("ping") => self.answer_ping(&msg),
            Some("cancel") => {
                let reply = msg["reply"].as_str().unwrap_or("");
                self.queue.retain(|r| r.reply != reply);
                if self.pending.as_ref().is_some_and(|r| r.reply == reply) {
                    self.pending = None;
                    self.show_next();
                }
            }
            Some("req") => {
                let reply = msg["reply"].as_str().unwrap_or("").to_string();
                let Some(key) = msg["k"].as_str().and_then(rv::import_session_key) else { return };
                if !is_hex64(&reply) || self.pending.as_ref().is_some_and(|r| r.reply == reply) || self.queue.iter().any(|r| r.reply == reply) {
                    return;
                }
                if self.hosts.len() + self.queue.len() + self.pending.is_some() as usize >= MAX_HOST {
                    self.send(&reply, &key, json!({ "t": "busy" }));
                    return;
                }
                let native = msg["caps"].as_array().is_some_and(|c| c.iter().any(|x| x == "mdp1"));
                let from = msg["from"].as_str().filter(|f| f.len() == 9 && f.chars().all(|c| c.is_ascii_digit())).unwrap_or("").to_string();
                // Switch sides: the computer we offered ours to connects back; no prompt.
                if native && self.is_switch_request(&msg) {
                    let req = Request { reply, key, peer: pubkey, name: msg["name"].as_str().map(|n| n.chars().take(60).collect()).unwrap_or_else(|| t("host.unknown")), os: os_name(msg["os"].as_str().unwrap_or("")).to_string(), from, native, host_key: None, at: Instant::now() };
                    self.start_host(req, Permissions { restart: false, privacy: false, tunnel: false, ..Permissions::default() }, false);
                    return;
                }
                // Access control list: only listed IDs get as far as the prompt.
                if self.store.acl_enabled && !self.store.acl.iter().any(|a| rv::normalize_id(a) == from) {
                    self.send(&reply, &key, json!({ "t": "reject", "reason": "acl" }));
                    return;
                }
                self.queue.push_back(Request {
                    reply,
                    key,
                    peer: pubkey,
                    name: msg["name"].as_str().map(|n| n.chars().take(60).collect()).unwrap_or_else(|| t("host.unknown")),
                    os: os_name(msg["os"].as_str().unwrap_or("")).to_string(),
                    from,
                    native,
                    at: Instant::now(),
                    host_key: None,
                });
                // Unattended access: start the session right away; it waits for the password
                // while the prompt still lets someone at this computer accept.
                let un = &self.store.unattended;
                if native && un.enabled && !un.key.is_empty() {
                    let req = self.queue.back().unwrap().clone();
                    let hk = self.start_host(req, Permissions::NONE, true);
                    self.queue.back_mut().unwrap().host_key = Some(hk);
                }
                self.show_next();
            }
            _ => {}
        }
    }

    fn show_next(&mut self) {
        if self.pending.is_none() {
            if let Some(mut r) = self.queue.pop_front() {
                r.at = Instant::now();
                self.pending = Some(r);
                bus::wake();
            }
        }
    }

    /// The Accept / Decline prompt.
    pub fn decide(&mut self, accept: bool, perms: Permissions) {
        let Some(req) = self.pending.take() else { return };
        if let Some(hk) = req.host_key {
            // Unattended access: the session is already up and waits for the password.
            if accept {
                self.authorize(hk, perms, false);
            } else if self.hosts.iter().any(|h| h.key == hk && h.auth.is_some()) {
                self.ctl(self.hosts.iter().find(|h| h.key == hk).and_then(|h| h.sid), json!({ "t": "reject" }));
                self.close_host(hk, true);
            }
            self.show_next();
            return;
        }
        if !accept {
            self.send(&req.reply, &req.key, json!({ "t": "reject" }));
        } else if !req.native {
            self.send(&req.reply, &req.key, json!({ "t": "reject", "reason": t("err.oldPeerHost") }));
            self.toast(format!("{}: {}", req.name, t("err.oldPeer")), true);
        } else {
            self.start_host(req, perms, false);
        }
        self.show_next();
    }

    /// `hold`: unattended access — no picture or input until the password is proven.
    /// Returns the host key.
    fn start_host(&mut self, req: Request, perms: Permissions, hold: bool) -> u32 {
        if !self.perms.screen {
            perms::request(true);
        }
        let key = self.next_id();
        let sid = self.next_id();
        let used: Vec<[u8; 3]> = self.hosts.iter().map(|h| h.color).collect();
        let color = VIEWER_COLORS.iter().find(|c| !used.contains(c)).copied().unwrap_or(VIEWER_COLORS[key as usize % 5]);
        let shown = self.screens.iter().find(|s| Some(s.id) == self.screen).or_else(|| self.screens.first()).cloned();
        let screen = shown.as_ref().map(|s| s.id);
        let stun = self.store.stun.clone();
        let turn_cfg = self.store.turn.clone();
        let (reg, rv, name) = (self.sessions.clone(), self.rv.clone(), self.name.clone());
        let r = req.clone();
        let player = Arc::new(Player::start());
        let pl = player.clone();
        crate::rt::spawn(async move {
            let events = Arc::new(move |ev: Value| bus::emit("mdp", json!({ "sid": sid, "ev": ev })));
            let media = Arc::new(move |m: Media| match m {
                Media::File(d) => bus::file(sid, d),
                Media::Audio(p) => pl.push(p),
                Media::Video { .. } => {}
            });
            let reject = |e: String| {
                let obj = json!({ "t": "reject", "reason": e, "ts": rv::now_ms() });
                (r.reply.clone(), rv::seal(&r.key, &obj))
            };
            let (s, prep) = match Session::prepare(true, stun, relay_server(turn_cfg).await, events, media).await {
                Ok(x) => x,
                Err(e) => {
                    let (topic, content) = reject(e.clone());
                    rv.publish(&topic, &content).await;
                    return bus::emit("host:offered", json!({ "host": key, "error": e }));
                }
            };
            let (w, h) = s.add_video(screen).await;
            if hold {
                s.hold_video();
            }
            reg.lock().unwrap().insert(sid, s.clone());
            let code = rv::encode_code(&json!({ "v": 1, "t": "offer", "mdp": prep }));
            let topic = rv::random_topic();
            rv.subscribe(&topic, false);
            // Register the answer topic before sending: the viewer can answer before the
            // relays have even acknowledged the offer.
            bus::emit("host:offered", json!({ "host": key, "sid": sid, "topic": topic, "w": w, "h": h }));
            let obj = json!({ "t": "offer", "code": code, "reply": topic, "name": name, "os": platform(), "ts": rv::now_ms() });
            rv.publish(&r.reply, &rv::seal(&r.key, &obj)).await;
        });
        self.hosts.push(Host {
            key,
            req,
            perms: if hold { Permissions::NONE } else { perms },
            auth: hold.then(|| AuthWait { nonce: hex::encode(rand::random::<[u8; 16]>()), tries: 0, since: Instant::now() }),
            unattended: false,
            live_since: None,
            color,
            hs_topic: None,
            hs_deadline: Instant::now() + Duration::from_secs(60),
            sid: None,
            state: HostState::Starting,
            sas: String::new(),
            chat: Chat::default(),
            quality: Quality::new(),
            src: (0, 0),
            last_tick: Instant::now(),
            stats: String::new(),
            stats_prev: None,
            incoming: None,
            remote_mic: false,
            player: Some(player),
            mic: None,
            pointer: None,
            screen: shown,
            fm_cancel: HashMap::new(),
            privacy: false,
            lock_on_end: false,
            strokes: Vec::new(),
            no_picture_sent: false,
        });
        key
    }

    fn on_host_offered(&mut self, v: Value) {
        if std::env::var_os("MANYDESK_DEBUG").is_some() {
            eprintln!("[host] offered {v}");
        }
        let key = v["host"].as_u64().unwrap_or(0) as u32;
        let Some(h) = self.hosts.iter_mut().find(|h| h.key == key) else { return };
        if let Some(e) = v["error"].as_str() {
            let name = h.req.name.clone();
            self.toast(tf("host.failed", &[("who", &name), ("e", e)]), true);
            self.close_host(key, false);
            return;
        }
        h.sid = v["sid"].as_u64().map(|s| s as u32);
        h.hs_topic = v["topic"].as_str().map(String::from);
        h.src = (v["w"].as_u64().unwrap_or(1920) as u32, v["h"].as_u64().unwrap_or(1080) as u32);
        h.state = HostState::Offered;
    }

    fn on_answer(&mut self, i: usize, msg: Value) {
        if msg["t"] != "answer" {
            return;
        }
        let code = msg["code"].as_str().unwrap_or("");
        if let Some(topic) = self.hosts[i].hs_topic.take() {
            self.rv.unsubscribe(&topic);
        }
        let Some(peer) = rv::decode_code(code).and_then(|a| serde_json::from_value::<Peer>(a["mdp"].clone()).ok()) else {
            let key = self.hosts[i].key;
            self.toast(t("err.badAnswer"), true);
            return self.close_host(key, false);
        };
        let Some(s) = self.session(self.hosts[i].sid) else { return };
        self.hosts[i].sas = rv::security_code(s.fingerprint(), &peer.fp);
        s.connect(peer);
    }

    pub fn close_host(&mut self, key: u32, send_bye: bool) {
        let Some(i) = self.hosts.iter().position(|h| h.key == key) else { return };
        let h = self.hosts.remove(i);
        for c in h.fm_cancel.values() {
            c.store(true, Ordering::Relaxed);
        }
        self.on_host_closed(&h);
        if let Some(t0) = h.live_since {
            self.store.log(LogEntry { ts: rv::now_ms(), dir: "in".into(), id: h.req.from.clone(), name: h.req.name.clone(), secs: t0.elapsed().as_secs(), unattended: h.unattended });
        }
        if let Some(t) = h.hs_topic {
            self.rv.unsubscribe(&t);
        }
        if let Some(s) = self.sessions.lock().unwrap().remove(&h.sid.unwrap_or(0)) {
            let text = json!({ "t": "bye" }).to_string();
            crate::rt::spawn(async move {
                if send_bye {
                    let _ = s.send_text(false, &text).await;
                }
                s.close();
            });
        }
        self.update_host_input();
    }

    pub fn set_host_perms(&mut self, key: u32, perms: Permissions) {
        let Some(i) = self.hosts.iter().position(|h| h.key == key && h.auth.is_none()) else { return };
        self.hosts[i].perms = perms;
        self.send_perms(i);
        self.update_host_input();
    }

    fn send_perms(&mut self, i: usize) {
        let (sid, p) = (self.hosts[i].sid, self.hosts[i].perms);
        if let Some(s) = self.session(sid) {
            s.allow_tunnels(p.tunnel);
        }
        self.ctl(sid, json!({ "t": "perm", "control": p.control, "clipboard": p.clipboard, "p": p, "os": platform() }));
    }

    /// Lets a waiting unattended session in: by password (`by_password`) or because
    /// someone here pressed Accept.
    fn authorize(&mut self, key: u32, perms: Permissions, by_password: bool) {
        let Some(i) = self.hosts.iter().position(|h| h.key == key) else { return };
        if self.hosts[i].auth.take().is_none() {
            return;
        }
        self.hosts[i].perms = perms;
        self.hosts[i].unattended = by_password;
        // The prompt for this request is no longer needed.
        self.queue.retain(|r| r.host_key != Some(key));
        if self.pending.as_ref().is_some_and(|r| r.host_key == Some(key)) {
            self.pending = None;
            self.show_next();
        }
        let sid = self.hosts[i].sid;
        self.ctl(sid, json!({ "t": "authok" }));
        if let Some(s) = self.session(sid) {
            s.release_video();
        }
        if self.hosts[i].state == HostState::Live {
            self.host_live(i);
        }
    }

    /// The viewer is in: permissions, displays, quality, input.
    fn host_live(&mut self, i: usize) {
        self.hosts[i].live_since = Some(Instant::now());
        self.switch_live();
        // Accept may have come before the session existed (then authorize() could not
        // reach it): release the picture now. Harmless when it is already flowing.
        if let Some(s) = self.session(self.hosts[i].sid) {
            s.release_video();
        }
        self.send_perms(i);
        self.send_screens(i);
        self.apply_quality(i);
        self.update_host_input();
        let name = self.hosts[i].req.name.clone();
        let key = if self.hosts[i].unattended { "host.joinedPw" } else { "host.joined" };
        self.toast(tf(key, &[("who", &name)]), false);
        // Test hook: MANYDESK_TESTMIC=1 turns the host microphone on when a viewer joins.
        if std::env::var_os("MANYDESK_TESTMIC").is_some() {
            let key = self.hosts[i].key;
            self.set_mic(None, Some(key), true);
            eprintln!("[test] host mic on: {}", self.hosts.iter().any(|h| h.mic.is_some()));
        }
    }

    fn update_host_input(&mut self) {
        if self.privacy_on || self.hosts.iter().any(|h| h.state == HostState::Live && h.perms.control) {
            self.input.start();
        } else {
            self.input.stop();
        }
    }

    fn apply_quality(&mut self, i: usize) {
        let h = &mut self.hosts[i];
        if let Some((scale, fps, br)) = h.quality.params(h.src) {
            if let Some(s) = self.sessions.lock().unwrap().get(&h.sid.unwrap_or(0)) {
                s.video_params(scale, fps, br);
            }
        }
    }

    // ------------------------------------------------- session events ---

    fn on_mdp(&mut self, sid: u32, ev: &Value) {
        let e = ev["e"].as_str().unwrap_or("");
        if let Some(i) = self.tabs.iter().position(|t| t.sid == Some(sid)) {
            match e {
                "open" => {
                    let tab = &mut self.tabs[i];
                    tab.state = TabState::Live;
                    tab.live_since = Some(Instant::now());
                    let (id, name, os) = (tab.peer_id.clone(), tab.label.clone(), tab.os.clone());
                    self.end_handshake(i);
                    self.store.remember(&id, &name, &os);
                    self.presence.insert(id, (Instant::now(), false));
                    // We draw the host's pointer ourselves when it can send it apart (sharper,
                    // and a pointer move costs no video frame). Older hosts ignore this.
                    self.ctl(self.tabs[i].sid, json!({ "t": "cursor" }));
                    // Test hook: MANYDESK_TESTFM=1 runs the file manager through its paces.
                    if std::env::var_os("MANYDESK_TESTFM").is_some() {
                        test_fm_later(self.tabs[i].key, 0, 3);
                    }
                    // Test hook: MANYDESK_TESTTUN=host:port opens a tunnel 127.0.0.1:0->that.
                    if let Ok(spec) = std::env::var("MANYDESK_TESTTUN") {
                        let key = self.tabs[i].key;
                        crate::rt::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(3)).await;
                            bus::emit("test:tun", json!({ "tab": key, "spec": spec }));
                        });
                    }
                    // Test hook: MANYDESK_TESTINPUT=1 moves the remote pointer once control is granted.
                    if std::env::var_os("MANYDESK_TESTINPUT").is_some() {
                        let sid = self.tabs[i].sid;
                        let reg = self.sessions.clone();
                        crate::rt::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(3)).await;
                            let Some(s) = sid.and_then(|id| reg.lock().unwrap().get(&id).cloned()) else { return };
                            for (x, y) in [(0.25, 0.25), (0.5, 0.5), (0.75, 0.3), (0.4, 0.6), (0.6, 0.4)] {
                                let _ = s.send_text(false, &json!({ "t": "move", "x": x, "y": y }).to_string()).await;
                                eprintln!("[test] moved to {x},{y}");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                            }
                            let _ = s.send_text(false, &json!({ "t": "scroll", "dx": 0, "dy": 0 }).to_string()).await;
                        });
                    }
                }
                "ctl" => {
                    if let Ok(m) = serde_json::from_str::<Value>(ev["text"].as_str().unwrap_or("")) {
                        self.viewer_ctl(i, m);
                    }
                }
                "file" => self.file_text(sid, ev["text"].as_str().unwrap_or("")),
                "closed" => {
                    let reason = reason_text(ev["reason"].as_str().unwrap_or(""));
                    let tab = &mut self.tabs[i];
                    let was_live = tab.state == TabState::Live;
                    tab.state = TabState::Failed(reason.clone());
                    self.log_tab(i);
                    self.sessions.lock().unwrap().remove(&sid);
                    if was_live {
                        let label = self.tabs[i].label.clone();
                        self.toast(format!("{label}: {reason}"), false);
                    }
                    if self.capture_tab == Some(self.tabs[i].key) {
                        self.set_capture(None);
                    }
                }
                _ => {}
            }
        } else if let Some(i) = self.hosts.iter().position(|h| h.sid == Some(sid)) {
            match e {
                "open" => {
                    self.hosts[i].state = HostState::Live;
                    match &self.hosts[i].auth {
                        Some(a) => {
                            // Unattended access: ask for the password first.
                            let un = &self.store.unattended;
                            let msg = json!({ "t": "auth", "nonce": a.nonce, "salt": un.salt, "totp": !un.totp.is_empty(), "os": platform() });
                            self.ctl(self.hosts[i].sid, msg);
                        }
                        None => self.host_live(i),
                    }
                }
                "ctl" => {
                    if let Ok(m) = serde_json::from_str::<Value>(ev["text"].as_str().unwrap_or("")) {
                        self.host_ctl(i, m);
                    }
                }
                "file" => self.file_text(sid, ev["text"].as_str().unwrap_or("")),
                "closed" => {
                    let (key, name, live) = (self.hosts[i].key, self.hosts[i].req.name.clone(), self.hosts[i].state == HostState::Live);
                    let reason = reason_text(ev["reason"].as_str().unwrap_or(""));
                    self.toast(format!("{name}: {}", if live { t("sess.ended") } else { reason }), !live);
                    self.close_host(key, false);
                }
                _ => {}
            }
        }
    }

    fn viewer_ctl(&mut self, i: usize, m: Value) {
        if matches!(m["t"].as_str(), Some("fls" | "fok" | "ferr")) {
            self.viewer_fm(i, &m);
            return;
        }
        if self.viewer_action(i, &m) {
            return;
        }
        let tab = &mut self.tabs[i];
        match m["t"].as_str() {
            Some("chat") => tab.chat.receive(m["text"].as_str().unwrap_or("")),
            Some("mic") => tab.remote_mic = m["on"].as_bool().unwrap_or(false),
            Some("cur") => {
                tab.pointer = m["x"].as_f64().zip(m["y"].as_f64()).map(|(x, y)| (x as f32, y as f32));
                crate::bus::wake();
            }
            Some("curimg") => {
                use base64::Engine;
                let png = base64::engine::general_purpose::STANDARD.decode(m["png"].as_str().unwrap_or("")).unwrap_or_default();
                if let Ok(img) = image::load_from_memory_with_format(&png, image::ImageFormat::Png) {
                    let img = img.to_rgba8();
                    let f = |k: &str| m[k].as_f64().unwrap_or(0.0) as f32;
                    tab.pointer_shape = Some(RemoteCursor {
                        image: egui::ColorImage::from_rgba_premultiplied([img.width() as usize, img.height() as usize], img.as_raw()),
                        size: egui::Vec2::new(f("w"), f("h")),
                        hot: egui::Vec2::new(f("hx"), f("hy")),
                    });
                    tab.pointer_gen = tab.pointer_gen.wrapping_add(1);
                }
            }
            Some("auth") => {
                tab.os = m["os"].as_str().unwrap_or(&tab.os).to_string();
                tab.auth = Some(AuthAsk {
                    nonce: m["nonce"].as_str().unwrap_or("").to_string(),
                    salt: m["salt"].as_str().unwrap_or("").to_string(),
                    totp: m["totp"].as_bool().unwrap_or(false),
                    remember: true,
                    ..Default::default()
                });
                let key = tab.key;
                self.try_saved_auth(key);
                // Test hook: MANYDESK_TESTPASSWORD=<pw> answers the password prompt.
                if let Ok(pw) = std::env::var("MANYDESK_TESTPASSWORD") {
                    if let Some(a) = self.tabs.iter_mut().find(|t| t.key == key).and_then(|t| t.auth.as_mut()).filter(|a| !a.busy) {
                        a.password = pw;
                        self.send_auth(key);
                    }
                }
            }
            Some("authfail") => {
                if let Some(a) = &mut tab.auth {
                    a.busy = false;
                    a.nonce = m["nonce"].as_str().unwrap_or(&a.nonce).to_string();
                    a.error = tf("auth.wrong", &[("n", &m["left"].as_u64().unwrap_or(0).to_string())]);
                }
                let id = tab.peer_id.clone();
                if let Some(r) = self.store.recent.iter_mut().find(|r| r.id == id) {
                    if r.auth.take().is_some() {
                        self.store.save();
                    }
                }
            }
            Some("authok") => {
                let (key, id) = (tab.key, tab.peer_id.clone());
                let ask = tab.auth.take();
                if let (Some(a), Some(k)) = (ask, self.auth_keys.remove(&key)) {
                    if let Some(r) = self.store.recent.iter_mut().find(|r| r.id == id) {
                        r.auth = a.remember.then(|| (a.salt.clone(), k));
                        self.store.save();
                    }
                }
            }
            Some("reject") => {
                let key = tab.key;
                tab.state = TabState::Failed(t("req.rejected"));
                if self.capture_tab == Some(key) {
                    self.set_capture(None);
                }
            }
            Some("screens") => {
                if std::env::var_os("MANYDESK_DEBUG").is_some() {
                    eprintln!("[screens] {m}");
                }
                tab.screens = m["list"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        Some(RemoteScreen {
                            id: s["id"].as_u64()? as u32,
                            name: s["name"].as_str().unwrap_or("").to_string(),
                            width: s["w"].as_u64().unwrap_or(0) as u32,
                            height: s["h"].as_u64().unwrap_or(0) as u32,
                        })
                    })
                    .collect();
                tab.screen = m["cur"].as_u64().map(|v| v as u32);
                // Test hook: MANYDESK_TESTSCREEN=1 switches to the next display 4 s after the list arrives.
                if std::env::var_os("MANYDESK_TESTSCREEN").is_some() && tab.screens.len() > 1 {
                    let key = tab.key;
                    crate::rt::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(4)).await;
                        bus::emit("test:screen", json!({ "tab": key }));
                    });
                }
            }
            Some("perm") => {
                tab.remote = serde_json::from_value(m["p"].clone()).unwrap_or(Permissions {
                    control: m["control"].as_bool().unwrap_or(false),
                    clipboard: m["clipboard"].as_bool().unwrap_or(false),
                    ..Permissions::NONE
                });
                tab.os = m["os"].as_str().unwrap_or(&tab.os).to_string();
                if !tab.remote.control && self.capture_tab == Some(tab.key) {
                    self.set_capture(None);
                }
            }
            Some("clip") => {
                if tab.remote.clipboard && self.active == Some(tab.key) {
                    if let Some(text) = m["text"].as_str() {
                        self.clip_last = text.to_string();
                        clip::set_text(text);
                    }
                }
            }
            Some("bye") => {
                let key = tab.key;
                tab.state = TabState::Failed(t("peer.ended"));
                if self.capture_tab == Some(key) {
                    self.set_capture(None);
                }
            }
            _ => {}
        }
    }

    /// Host: tells the viewer which displays there are and which one it sees.
    fn send_screens(&mut self, i: usize) {
        self.refresh_screens(false);
        let list: Vec<Value> = self.screens.iter().map(|s| json!({ "id": s.id, "name": s.name, "w": s.width, "h": s.height })).collect();
        let cur = self.hosts[i].screen.as_ref().map(|s| s.id);
        self.ctl(self.hosts[i].sid, json!({ "t": "screens", "list": list, "cur": cur }));
    }

    fn host_ctl(&mut self, i: usize, m: Value) {
        if self.hosts[i].auth.is_some() {
            // Until the password is proven only "auth" and "bye" count.
            let (key, sid) = (self.hosts[i].key, self.hosts[i].sid);
            match m["t"].as_str() {
                Some("auth") => {
                    let Some(s) = self.session(sid) else { return };
                    let un = &self.store.unattended;
                    let nonce = self.hosts[i].auth.as_ref().map(|a| a.nonce.clone()).unwrap_or_default();
                    let ok = crate::auth::verify(&un.key, &un.totp, &nonce, s.fingerprint(), &s.peer_fingerprint(), m["proof"].as_str().unwrap_or(""));
                    if ok {
                        let perms = un.perms;
                        self.authorize(key, perms, true);
                        return;
                    }
                    let a = self.hosts[i].auth.as_mut().unwrap();
                    a.tries += 1;
                    let left = 3u8.saturating_sub(a.tries);
                    // a fresh nonce for each try
                    a.nonce = hex::encode(rand::random::<[u8; 16]>());
                    let msg = json!({ "t": "authfail", "left": left, "nonce": a.nonce });
                    self.ctl(sid, msg);
                    if left == 0 {
                        let name = self.hosts[i].req.name.clone();
                        self.toast(tf("auth.blocked", &[("who", &name)]), true);
                        self.close_host(key, true);
                    }
                }
                Some("bye") => self.close_host(key, false),
                _ => {}
            }
            return;
        }
        match m["t"].as_str() {
            Some("screen") => {
                let Some(id) = m["id"].as_u64().map(|v| v as u32) else { return };
                let (key, sid) = (self.hosts[i].key, self.hosts[i].sid);
                let Some(s) = self.session(sid) else { return };
                crate::rt::spawn(async move {
                    match s.switch_screen(id).await {
                        Some(screen) => bus::emit("host:screen", json!({ "host": key, "screen": serde_json::to_value(&screen).unwrap_or_default() })),
                        None => bus::emit("host:screen", json!({ "host": key })),
                    }
                });
            }
            Some("cursor") => {
                if let Some(s) = self.session(self.hosts[i].sid) {
                    s.cursor_apart();
                }
            }
            Some("chat") => self.hosts[i].chat.receive(m["text"].as_str().unwrap_or("")),
            Some("mic") => self.hosts[i].remote_mic = m["on"].as_bool().unwrap_or(false),
            Some("quality") => {
                if let Some(mode) = m["mode"].as_str().and_then(Mode::parse) {
                    self.hosts[i].quality.set_mode(mode);
                    self.apply_quality(i);
                }
            }
            Some("view") => {
                self.hosts[i].quality.set_view(m["w"].as_f64().unwrap_or(0.0), m["h"].as_f64().unwrap_or(0.0));
                self.apply_quality(i);
            }
            Some("clip") => {
                if self.hosts[i].perms.clipboard {
                    if let Some(text) = m["text"].as_str() {
                        self.clip_last = text.to_string();
                        clip::set_text(text);
                    }
                }
            }
            Some("bye") => {
                let key = self.hosts[i].key;
                self.close_host(key, false);
            }
            Some(t @ ("fls" | "fget" | "fcancel" | "fmk" | "frm" | "fmv")) => self.host_fm(i, t, &m),
            Some(t @ ("restart" | "privacy" | "special" | "lockend" | "draw" | "drawclear" | "switch")) => {
                self.host_action(i, t, &m);
            }
            Some(kind) if self.hosts[i].perms.control => {
                let kind: &'static str = match kind {
                    "down" => "down",
                    "scroll" => "scroll",
                    "key" | "text" => "key",
                    "up" => "up",
                    _ => "move",
                };
                let pos = self.hosts[i].screen.clone().and_then(|sc| self.input.handle(&sc, &m));
                let now = Instant::now();
                let h = &mut self.hosts[i];
                match (&mut h.pointer, pos) {
                    (Some(p), _) => {
                        if let Some((x, y)) = pos {
                            (p.x, p.y) = (x as f32, y as f32);
                        }
                        if kind != "up" {
                            p.kind = kind;
                        }
                        if kind == "down" {
                            p.click = Some(now);
                        }
                        p.at = now;
                    }
                    (None, Some((x, y))) => {
                        h.pointer = Some(Pointer { x: x as f32, y: y as f32, kind, at: now, click: (kind == "down").then_some(now) });
                    }
                    (None, None) => {}
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------- files ---

    pub fn send_files(&mut self, sid: Option<u32>, paths: Vec<PathBuf>) {
        let Some(s) = self.session(sid) else { return };
        crate::rt::spawn(async move {
            for p in paths {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                let Ok(data) = tokio::fs::read(&p).await else { continue };
                let id = rv::random_topic()[..16].to_string();
                let start = json!({ "t": "fstart", "id": id, "name": name, "size": data.len() }).to_string();
                if s.send_text(true, &start).await.is_err() {
                    break;
                }
                for chunk in data.chunks(64 * 1024) {
                    if s.send_file_bytes(chunk).await.is_err() {
                        break;
                    }
                }
                let _ = s.send_text(true, &json!({ "t": "fend", "id": id }).to_string()).await;
                bus::emit("toast", json!({ "text": tf("file.sent", &[("who", ""), ("f", &name)]).trim_start_matches(": ").to_string() }));
            }
        });
    }

    fn incoming_of(&mut self, sid: u32) -> Option<&mut Option<Incoming>> {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.sid == Some(sid)) {
            return Some(&mut t.incoming);
        }
        self.hosts.iter_mut().find(|h| h.sid == Some(sid)).map(|h| &mut h.incoming)
    }

    fn file_text(&mut self, sid: u32, text: &str) {
        let Ok(m) = serde_json::from_str::<Value>(text) else { return };
        // A host takes files only with the permission (clipboard pictures: see clip.rs).
        let clip = m["fm"].as_str().is_some_and(|b| b.starts_with("clip-"));
        if !clip && self.hosts.iter().any(|h| h.sid == Some(sid) && !h.perms.files) {
            return;
        }
        if m["fm"].is_string() {
            self.fm_file_text(sid, &m);
            return;
        }
        let mut done: Option<(String, PathBuf)> = None;
        if let Some(slot) = self.incoming_of(sid) {
            match m["t"].as_str() {
                Some("fstart") => {
                    let name = sanitize(m["name"].as_str().unwrap_or("file"));
                    let path = unique(store::downloads().join(&name));
                    if let Ok(file) = std::fs::File::create(&path) {
                        *slot = Some(Incoming { id: m["id"].as_str().unwrap_or("").into(), name, file, path, batch: None });
                    }
                }
                Some("fend") => {
                    if slot.as_ref().is_some_and(|i| m["id"] == i.id.as_str()) {
                        let inc = slot.take().unwrap();
                        done = Some((inc.name, inc.path));
                    }
                }
                _ => {}
            }
        }
        if let Some((name, path)) = done {
            self.toasts.push(Toast { text: tf("file.received", &[("who", ""), ("f", &name)]).trim_start_matches(": ").into(), error: false, until: Instant::now() + Duration::from_secs(10), path: Some(path) });
        }
    }

    fn on_file_bytes(&mut self, sid: u32, data: Vec<u8>) {
        use std::io::Write;
        let Some(Some(inc)) = self.incoming_of(sid) else { return };
        let _ = inc.file.write_all(&data);
        let batch = inc.batch.clone();
        if let (Some(b), Some(tab)) = (batch, self.tabs.iter_mut().find(|t| t.sid == Some(sid))) {
            if let Some(tr) = tab.fm.transfer(&b) {
                tr.done += data.len() as u64;
            }
        }
    }

    /// A file manager copy arriving: downloads (viewer) or uploads (host).
    fn fm_file_text(&mut self, sid: u32, m: &Value) {
        let batch = m["fm"].as_str().unwrap_or("").to_string();
        let tab = self.tabs.iter().position(|t| t.sid == Some(sid));
        let host = self.hosts.iter().position(|h| h.sid == Some(sid));
        // where the batch goes: the folder the viewer picked, or the one an upload names
        let clip = batch.starts_with("clip-");
        let base = match (tab, host) {
            _ if clip => self.clip_allowed(sid, &batch).then(|| clip::clip_dir(&batch)).flatten(),
            (Some(i), _) => self.tabs[i].fm.dests.get(&batch).cloned(),
            (_, Some(_)) => m["dir"].as_str().map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir()),
            _ => None,
        };
        match m["t"].as_str() {
            Some("fstart") => {
                if let Some(i) = tab {
                    if let Some(tr) = self.tabs[i].fm.transfer(&batch) {
                        tr.total = m["total"].as_u64().unwrap_or(tr.total);
                    }
                }
                let Some(slot) = self.incoming_of(sid) else { return };
                *slot = None;
                let rel = m["rel"].as_str().unwrap_or("");
                let empty_dir = m["d"].as_bool().unwrap_or(false);
                let Some(path) = base.and_then(|b| fm::target(&b, rel, empty_dir)) else { return };
                if empty_dir {
                    let _ = std::fs::create_dir_all(&path);
                } else if let Ok(file) = std::fs::File::create(&path) {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    *slot = Some(Incoming { id: batch, name, file, path, batch: Some(m["fm"].as_str().unwrap_or("").into()) });
                }
            }
            Some("fend") => {
                let Some(slot) = self.incoming_of(sid) else { return };
                if slot.as_ref().is_some_and(|inc| inc.id == batch) {
                    let inc = slot.take().unwrap();
                    if m["x"].as_bool().unwrap_or(false) {
                        drop(inc.file);
                        let _ = std::fs::remove_file(&inc.path);
                    }
                }
            }
            Some("fdone") => {
                let (cancelled, e) = (m["x"].as_bool().unwrap_or(false), m["e"].as_str().unwrap_or("").to_string());
                if clip {
                    if !cancelled && e.is_empty() && self.clip_allowed(sid, &batch) {
                        self.clip_received(&batch);
                    }
                    return;
                }
                if let Some(i) = tab {
                    let f = &mut self.tabs[i].fm;
                    f.dests.remove(&batch);
                    let mut dest = None;
                    if let Some(tr) = f.transfer(&batch) {
                        tr.finished = true;
                        tr.cancelled = cancelled;
                        tr.error = e;
                        tr.done = tr.done.max(if cancelled { 0 } else { tr.total });
                        dest = Some(tr.dest.clone());
                    }
                    if dest.is_some_and(|d| d == f.local.path) {
                        let p = f.local.path.clone();
                        f.list_local(&p);
                    }
                } else if let (Some(i), Some(dir)) = (host, m["dir"].as_str()) {
                    if !cancelled && e.is_empty() {
                        let who = self.hosts[i].req.name.clone();
                        let dir = dir.to_string();
                        self.toasts.push(Toast { text: tf("fm.hostPut", &[("who", &who), ("dir", &dir)]), error: false, until: Instant::now() + Duration::from_secs(10), path: Some(PathBuf::from(dir)) });
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------- file manager: host ---

    /// The viewer browses and copies from this computer (needs the files permission).
    fn host_fm(&mut self, i: usize, kind: &str, m: &Value) {
        let rid = m["rid"].as_str().unwrap_or("").to_string();
        let Some(s) = self.session(self.hosts[i].sid) else { return };
        if !self.hosts[i].perms.files {
            self.ctl(self.hosts[i].sid, json!({ "t": "ferr", "rid": rid, "e": t("fm.denied") }));
            return;
        }
        let strs = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        match kind {
            "fls" => {
                let path = m["path"].as_str().unwrap_or("").to_string();
                crate::rt::spawn(async move {
                    let reply = tokio::task::spawn_blocking(move || fm::list_reply(&rid, &path)).await.unwrap_or_default();
                    let _ = s.send_text(false, &reply.to_string()).await;
                });
            }
            "fget" => {
                let paths: Vec<PathBuf> = strs(&m["paths"]).into_iter().map(PathBuf::from).filter(|p| p.is_absolute()).collect();
                let who = self.hosts[i].req.name.clone();
                self.toast(tf("fm.hostGet", &[("who", &who), ("n", &paths.len().to_string())]), false);
                let cancel = Arc::new(AtomicBool::new(false));
                self.hosts[i].fm_cancel.insert(rid.clone(), cancel.clone());
                crate::rt::spawn(async move {
                    let _ = fm::send_batch(s, paths, rid, None, cancel, |_, _| {}).await;
                });
            }
            "fcancel" => {
                if let Some(c) = self.hosts[i].fm_cancel.remove(&rid) {
                    c.store(true, Ordering::Relaxed);
                }
            }
            _ => {
                let op: Box<dyn FnOnce() -> std::io::Result<()> + Send> = match kind {
                    "fmk" => {
                        let p = PathBuf::from(m["path"].as_str().unwrap_or(""));
                        Box::new(move || if p.is_absolute() { std::fs::create_dir(&p) } else { Err(std::io::ErrorKind::InvalidInput.into()) })
                    }
                    "frm" => {
                        let ps: Vec<PathBuf> = strs(&m["paths"]).into_iter().map(PathBuf::from).filter(|p| p.is_absolute()).collect();
                        Box::new(move || ps.iter().try_for_each(|p| fm::remove(p)))
                    }
                    _ => {
                        let (from, to) = (PathBuf::from(m["from"].as_str().unwrap_or("")), PathBuf::from(m["to"].as_str().unwrap_or("")));
                        Box::new(move || {
                            if !from.is_absolute() || !to.is_absolute() || to.exists() {
                                return Err(std::io::ErrorKind::AlreadyExists.into());
                            }
                            std::fs::rename(&from, &to)
                        })
                    }
                };
                crate::rt::spawn(async move {
                    let r = tokio::task::spawn_blocking(op).await.unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
                    let reply = match r {
                        Ok(()) => json!({ "t": "fok", "rid": rid }),
                        Err(e) => json!({ "t": "ferr", "rid": rid, "e": e.to_string() }),
                    };
                    let _ = s.send_text(false, &reply.to_string()).await;
                });
            }
        }
    }

    // ------------------------------------------------- file manager: viewer ---

    fn viewer_fm(&mut self, i: usize, m: &Value) {
        let (key, rid) = (self.tabs[i].key, m["rid"].as_str().unwrap_or("").to_string());
        let f = &mut self.tabs[i].fm;
        match m["t"].as_str() {
            Some("fls") => {
                f.remote = Pane::from_reply(m);
            }
            Some("ferr") => {
                let e = m["e"].as_str().unwrap_or("").to_string();
                if let Some(tr) = f.transfer(&rid) {
                    tr.finished = true;
                    tr.error = e;
                } else {
                    f.remote.loading = false;
                    f.remote.error = e;
                }
            }
            _ => {
                // fok: a remote change went through; show it
                let path = f.remote.path.clone();
                self.fm_ls(key, &path);
            }
        }
    }

    /// Opens or closes the file manager (first open lists both home folders).
    pub fn fm_toggle(&mut self, key: u32) {
        let Some(tab) = self.tab(key) else { return };
        tab.fm.open = !tab.fm.open;
        if !tab.fm.open {
            return;
        }
        if tab.fm.local.path.is_empty() {
            tab.fm.list_local("");
        }
        if tab.fm.remote.path.is_empty() && !tab.fm.remote.loading {
            self.fm_ls(key, "");
        }
        if self.capture_tab == Some(key) {
            self.set_capture(None);
        }
    }

    /// Asks the host for a folder ("" = its home).
    pub fn fm_ls(&mut self, key: u32, path: &str) {
        let Some(tab) = self.tab(key) else { return };
        let rid = tab.fm.rid();
        tab.fm.remote.loading = true;
        tab.fm.remote.error.clear();
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "fls", "rid": rid, "path": path }));
    }

    pub fn fm_ls_local(&mut self, key: u32, path: &str) {
        if let Some(tab) = self.tab(key) {
            tab.fm.list_local(path);
        }
    }

    /// Copies `paths` (default: the selection) to the other side's open folder.
    pub fn fm_copy(&mut self, key: u32, upload: bool, paths: Option<Vec<String>>) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        let f = &mut tab.fm;
        let (sel, dest) = if upload { (f.local.selected_paths(), f.remote.path.clone()) } else { (f.remote.selected_paths(), f.local.path.clone()) };
        let paths = paths.unwrap_or(sel);
        if paths.is_empty() || dest.is_empty() {
            return;
        }
        let rid = f.rid();
        let name = fm::summary(&paths, |n| tf("fm.items", &[("n", &n.to_string())]));
        f.transfers.push(Transfer { id: rid.clone(), name, upload, dest: dest.clone(), ..Default::default() });
        if !upload {
            f.dests.insert(rid.clone(), PathBuf::from(&dest));
            self.ctl(sid, json!({ "t": "fget", "rid": rid, "paths": paths }));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        f.cancels.insert(rid.clone(), cancel.clone());
        let Some(s) = self.session(sid) else { return };
        let sid = sid.unwrap_or(0);
        crate::rt::spawn(async move {
            let id = rid.clone();
            let r = fm::send_batch(s, paths.into_iter().map(PathBuf::from).collect(), rid, Some(dest), cancel, |done, total| {
                bus::emit("fm:progress", json!({ "sid": sid, "id": id, "done": done, "total": total }));
            })
            .await;
            bus::emit("fm:done", json!({ "sid": sid, "id": id, "x": matches!(r, Ok(false)), "e": r.err().unwrap_or_default() }));
        });
    }

    /// Upload progress (reported by the sending task).
    fn on_fm_upload(&mut self, name: &str, v: &Value) {
        let sid = v["sid"].as_u64().map(|s| s as u32);
        let Some(i) = self.tabs.iter().position(|t| t.sid == sid) else { return };
        let (key, id) = (self.tabs[i].key, v["id"].as_str().unwrap_or(""));
        let f = &mut self.tabs[i].fm;
        let Some(tr) = f.transfer(id) else { return };
        if name == "fm:progress" {
            tr.done = v["done"].as_u64().unwrap_or(0);
            tr.total = v["total"].as_u64().unwrap_or(0);
            return;
        }
        tr.finished = true;
        tr.cancelled = v["x"].as_bool().unwrap_or(false);
        tr.error = v["e"].as_str().unwrap_or("").to_string();
        let dest = tr.dest.clone();
        f.cancels.remove(id);
        if dest == f.remote.path {
            self.fm_ls(key, &dest);
        }
    }

    pub fn fm_cancel(&mut self, key: u32, id: &str) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        let Some(tr) = tab.fm.transfer(id) else { return };
        if tr.finished {
            return;
        }
        if tr.upload {
            if let Some(c) = tab.fm.cancels.get(id) {
                c.store(true, Ordering::Relaxed);
            }
        } else {
            self.ctl(sid, json!({ "t": "fcancel", "rid": id }));
        }
    }

    /// New folder `name` in the open folder of one side.
    pub fn fm_mkdir(&mut self, key: u32, remote: bool, name: &str) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        let f = &mut tab.fm;
        if remote {
            let (rid, path) = (f.rid(), f.remote.join(name.trim()));
            self.ctl(sid, json!({ "t": "fmk", "rid": rid, "path": path }));
        } else {
            let path = f.local.join(name.trim());
            let r = std::fs::create_dir(&path);
            Self::fm_local_done(f, r);
        }
    }

    /// Deletes entries `names` of one side's open folder.
    pub fn fm_delete(&mut self, key: u32, remote: bool, names: &[String]) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        let f = &mut tab.fm;
        if remote {
            let (rid, paths): (String, Vec<String>) = (f.rid(), names.iter().map(|n| f.remote.join(n)).collect());
            self.ctl(sid, json!({ "t": "frm", "rid": rid, "paths": paths }));
        } else {
            let r = names.iter().try_for_each(|n| fm::remove(std::path::Path::new(&f.local.join(n))));
            Self::fm_local_done(f, r);
        }
    }

    pub fn fm_rename(&mut self, key: u32, remote: bool, old: &str, new: &str) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        let f = &mut tab.fm;
        if remote {
            let (rid, from, to) = (f.rid(), f.remote.join(old), f.remote.join(new.trim()));
            self.ctl(sid, json!({ "t": "fmv", "rid": rid, "from": from, "to": to }));
        } else {
            let (from, to) = (PathBuf::from(f.local.join(old)), PathBuf::from(f.local.join(new.trim())));
            let r = if to.exists() { Err(std::io::ErrorKind::AlreadyExists.into()) } else { std::fs::rename(&from, &to) };
            Self::fm_local_done(f, r);
        }
    }

    fn fm_local_done(f: &mut FileManager, r: std::io::Result<()>) {
        let path = f.local.path.clone();
        f.list_local(&path);
        if let Err(e) = r {
            f.local.error = e.to_string();
        }
    }

    // ------------------------------------------------- clipboard ---

    fn on_local_clipboard(&mut self, text: &str) {
        if text == self.clip_last || text.is_empty() || text.len() > 1 << 20 {
            return;
        }
        self.clip_last = text.to_string();
        let msg = json!({ "t": "clip", "text": text });
        for sid in self.clip_targets(false) {
            self.ctl(sid, msg.clone());
        }
    }

    // ------------------------------------------------- presence ---

    fn poll_presence(&mut self) {
        self.last_poll = Some(Instant::now());
        let ids: Vec<String> = self.store.recent.iter().map(|r| r.id.clone()).collect();
        self.ping_ids(ids);
    }

    /// Asks these computers whether they are online (answers arrive as "pong").
    fn ping_ids(&mut self, ids: Vec<String>) {
        let (rv, topic, key) = (self.rv.clone(), self.presence_topic.clone(), self.presence_raw.clone());
        crate::rt::spawn_blocking(move || {
            for id in ids {
                let obj = json!({ "t": "ping", "reply": topic, "k": key, "ts": rv::now_ms() });
                let content = rv::seal(&rv::key_for_id(&id), &obj);
                let (rv, t) = (rv.clone(), rv::topic_for_id(&id));
                crate::rt::spawn(async move {
                    rv.publish(&t, &content).await;
                });
            }
        });
    }

    fn on_pong(&mut self, msg: Value) {
        if msg["t"] != "pong" {
            return;
        }
        let id = msg["id"].as_str().map(String::from).or_else(|| msg["id"].as_u64().map(|n| n.to_string())).unwrap_or_default();
        let Some(r) = self.store.recent.iter_mut().find(|r| r.id == id) else { return };
        self.presence.insert(id, (Instant::now(), msg["busy"].as_bool().unwrap_or(false)));
        if let Some(n) = msg["name"].as_str().filter(|n| !n.is_empty() && *n != r.name) {
            r.name = n.chars().take(60).collect();
            self.store.save();
        }
    }

    fn answer_ping(&mut self, msg: &Value) {
        let reply = msg["reply"].as_str().unwrap_or("").to_string();
        if !self.store.presence || !is_hex64(&reply) {
            return;
        }
        let now = Instant::now();
        if self.pong_log.get(&reply).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(3)) {
            return;
        }
        if now.duration_since(self.pong_window.0) > Duration::from_secs(60) {
            self.pong_window = (now, 0);
        }
        self.pong_window.1 += 1;
        if self.pong_window.1 > 60 {
            return;
        }
        if self.pong_log.len() > 500 {
            self.pong_log.clear();
        }
        self.pong_log.insert(reply.clone(), now);
        let Some(key) = msg["k"].as_str().and_then(rv::import_session_key) else { return };
        let busy = self.hosts.len() >= MAX_HOST;
        self.send(&reply, &key, json!({ "t": "pong", "id": self.store.my_id, "name": self.name, "os": platform(), "busy": busy }));
    }

    pub fn new_id(&mut self) {
        self.rv.unsubscribe(&self.my_topic);
        self.store.my_id = rv::random_id();
        self.store.save();
        self.my_topic = rv::topic_for_id(&self.store.my_id);
        self.rv.subscribe(&self.my_topic, true);
        let id = self.store.my_id.clone();
        crate::rt::spawn_blocking(move || rv::key_for_id(&id));
    }

    // ------------------------------------------------- stats ---

    fn update_stats(&mut self) {
        let now = Instant::now();
        for i in 0..self.tabs.len() {
            if self.tabs[i].state != TabState::Live || self.tabs[i].stats_prev.is_some_and(|p| now.duration_since(p.1) < Duration::from_millis(1500)) {
                continue;
            }
            let Some(s) = self.session(self.tabs[i].sid) else { continue };
            let st = s.stats();
            let tab = &mut self.tabs[i];
            let (fps, size, dec) = tab.feed.as_ref().map(|f| (f.shared.fps(), *f.shared.size.lock().unwrap(), *f.shared.decoder.lock().unwrap())).unwrap_or_default();
            let recv = st["recv"].as_u64().unwrap_or(0);
            tab.stats = fmt_stats(&st, recv, &mut tab.stats_prev, fps, size, dec);
        }
        for i in 0..self.hosts.len() {
            if self.hosts[i].state != HostState::Live {
                continue;
            }
            let Some(s) = self.session(self.hosts[i].sid) else { continue };
            if now.duration_since(self.hosts[i].last_tick) >= Duration::from_secs(2) {
                self.hosts[i].last_tick = now;
                let st = s.stats();
                if self.hosts[i].quality.tick(&st["video"], st["rtt"].as_f64().unwrap_or(0.0), st["cwnd"].as_f64().unwrap_or(0.0)) {
                    self.apply_quality(i);
                }
                let h = &mut self.hosts[i];
                let v = &st["video"];
                let size = (v["frameWidth"].as_u64().unwrap_or(0) as usize, v["frameHeight"].as_u64().unwrap_or(0) as usize);
                let imp = v["encoderImplementation"].as_str().unwrap_or("");
                let sent = st["sent"].as_u64().unwrap_or(0);
                h.stats = fmt_stats(&st, sent, &mut h.stats_prev, v["framesPerSecond"].as_f64().unwrap_or(0.0), size, imp);
                // Live for a while and not one frame captured: the screen can't be read
                // here (macOS: Screen Recording permission). Say so on both ends.
                let frames = v["framesEncoded"].as_u64().unwrap_or(0);
                let waited = h.live_since.is_some_and(|t| now.duration_since(t) > Duration::from_secs(12));
                if frames == 0 && waited && h.auth.is_none() && !h.no_picture_sent {
                    h.no_picture_sent = true;
                    eprintln!("[host] no frame captured 12 s after the session started (screen recording permission?)");
                    let sid = h.sid;
                    self.ctl(sid, json!({ "t": "nopicture", "os": platform() }));
                    self.toast(t("cap.noPictureHost"), true);
                    self.perms = perms::status();
                    if cfg!(target_os = "macos") {
                        perms::request(true);
                    }
                }
            }
        }
    }
}

fn fmt_stats(st: &Value, bytes: u64, prev: &mut Option<(u64, Instant)>, fps: f64, size: (usize, usize), imp: &str) -> String {
    let now = Instant::now();
    let kbps = prev.map(|(b, t)| (bytes.saturating_sub(b) as f64 * 8.0) / now.duration_since(t).as_secs_f64() / 1000.0).unwrap_or(0.0);
    *prev = Some((bytes, now));
    let mut parts = vec![if st["relay"].as_bool().unwrap_or(false) {
        t("st.relay")
    } else if st["lan"].as_bool().unwrap_or(false) {
        t("st.lan")
    } else {
        "P2P".into()
    }];
    if let Some(rtt) = st["rtt"].as_f64() {
        parts.push(format!("{} ms", rtt.round()));
    }
    if size.0 > 0 {
        parts.push(format!("{}×{}", size.0, size.1));
    }
    parts.push(format!("{} fps", fps.round()));
    parts.push(if imp.is_empty() { "H264".into() } else { format!("H264 · {imp}") });
    parts.push(if kbps > 1000.0 { format!("{:.1} Mbit/s", kbps / 1000.0) } else { format!("{} kbit/s", kbps.round()) });
    let total = (st["sent"].as_u64().unwrap_or(0) + st["recv"].as_u64().unwrap_or(0)) as f64 / 1_048_576.0;
    parts.push(tf("st.total", &[("mb", &if total < 10.0 { format!("{total:.1}") } else { format!("{}", total.round()) })]));
    parts.join("  ·  ")
}

/// The TURN server for a new session; a problem is shown and the session goes without.
async fn relay_server(cfg: crate::store::TurnCfg) -> Option<crate::mdp::turn::Server> {
    match crate::turncfg::server(cfg).await {
        Ok(s) => s,
        Err(e) => {
            bus::emit("toast", json!({ "text": tf("turn.notReady", &[("e", &e)]), "error": true }));
            None
        }
    }
}

fn reason_text(r: &str) -> String {
    match r {
        "bye" => t("peer.left"),
        "lost" => t("conn.lost"),
        "noroute" => t("conn.noRoute"),
        "noroute-turn" => t("conn.noRouteTurn"),
        other => tf("conn.failed", &[("e", other)]),
    }
}

fn test_fm_later(key: u32, step: u64, secs: u64) {
    crate::rt::spawn(async move {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        bus::emit("test:fm", json!({ "tab": key, "step": step }));
    });
}

impl Core {
    /// MANYDESK_TESTFM: list, copy both ways, new folder, rename, delete, cancel — printed as "[testfm]".
    fn test_fm(&mut self, key: u32, step: u64) {
        let Some(tab) = self.tab(key) else { return };
        let f = &mut tab.fm;
        let names = |p: &Pane| p.entries.iter().map(|e| if e.d { format!("{}/", e.n) } else { e.n.clone() }).collect::<Vec<_>>().join(" ");
        eprintln!("[testfm] step {step}: local {} [{}] | remote {} [{}] err={:?}", f.local.path, names(&f.local), f.remote.path, names(&f.remote), f.remote.error);
        for t in &f.transfers {
            eprintln!("[testfm]   transfer {} {} up={} {}/{} finished={} cancelled={} err={:?}", t.id, t.name, t.upload, t.done, t.total, t.finished, t.cancelled, t.error);
        }
        let sel = |p: &mut Pane, want: &[&str]| p.selected = want.iter().map(|s| s.to_string()).collect();
        // wait for the host's listing, and for copies to finish
        if (step == 1 && f.remote.path.is_empty()) || (step == 2 && f.busy()) {
            return test_fm_later(key, step, 1);
        }
        match step {
            0 => self.fm_toggle(key),
            1 => {
                sel(&mut f.remote, &["Shared", "big.bin"]);
                sel(&mut f.local, &["Outbox"]);
                self.fm_copy(key, false, None);
                self.fm_copy(key, true, None);
                self.fm_mkdir(key, true, "made");
            }
            2 => self.fm_rename(key, true, "made", "renamed"),
            3 => self.fm_delete(key, true, &["renamed".to_string()]),
            4 => {
                sel(&mut f.remote, &["big.bin"]);
                self.fm_copy(key, false, None);
                let id = self.tab(key).and_then(|t| t.fm.transfers.last().map(|t| t.id.clone())).unwrap_or_default();
                self.fm_cancel(key, &id);
            }
            _ => return,
        }
        test_fm_later(key, step + 1, if step == 1 { 2 } else { 3 });
    }
}

fn sanitize(name: &str) -> String {
    let n: String = name.chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    let n = n.trim_start_matches('.').chars().take(200).collect::<String>();
    if n.is_empty() { "file".into() } else { n }
}

fn unique(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..1000).map(|i| p.with_file_name(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap_or(p)
}


