// The window, a 1:1 port of the web UI (src/renderer/index.html + styles.css): title bar
// with tabs and the address bar, the sidebar (this desk's address, navigation), the
// remote / sharing / settings pages, one session view per remote computer (toolbar,
// stage, connecting card, chat column), the incoming-request window, host session cards
// and toasts. Sizes, colours and fonts follow the CSS; see theme.rs.
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, Id, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2};
use serde_json::json;

use crate::bus::{self, Msg};
use crate::core::{Chat, Core, HostState, TabState, ViewMode};
use crate::i18n::{self, t, tf};
use crate::keys;
use crate::quality::Mode;
use crate::rv;
use crate::theme::{self as th, Btn, Icons, W};

mod actions;
mod term;
use actions::{ActDialog, Board};

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Remote,
    Share,
    Settings,
}

/// Whether windows can be transparent (false when the GL driver has no alpha surface).
pub static OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

type DiagState = Arc<Mutex<Option<(String, Option<bool>)>>>;

pub struct App {
    core: Core,
    rx: Receiver<Msg>,
    icons: Icons,
    page: Page,
    address: String,
    /// the address bar's suggestions (AnyDesk's autocomplete): the highlighted one, closed
    /// with Esc until the text changes, the pointer over the list (it keeps it open)
    ac_sel: Option<usize>,
    ac_closed: bool,
    ac_hover: bool,
    fullscreen: bool,
    tb_hide_at: Option<Instant>,
    thumbs: HashMap<String, (usize, TextureHandle, TextureHandle)>,
    screen_thumbs: HashMap<u32, TextureHandle>,
    req_perms: crate::store::Permissions,
    /// the request the toggles above were set up for (its time)
    req_seen: Option<Instant>,
    frames: HashMap<u32, usize>,
    mac_local: bool,
    host_chat: Option<u32>,
    collapsed: HashSet<u32>,
    /// host cards: when each went live, and the ones already folded by themselves (AnyDesk
    /// folds its session window to a bar once the session runs; the user may unfold it)
    host_live_at: HashMap<u32, Instant>,
    auto_folded: HashSet<u32>,
    /// displays whose overlay window is already kept out of screen capture
    overlay_hidden: Vec<u32>,
    diag: DiagState,
    stun_text: String,
    turn: crate::store::TurnCfg,
    turn_urls: String,
    turn_out: DiagState,
    un_pw: (String, String),
    /// a TOTP secret being set up (shown as QR until a code confirms it)
    totp_new: Option<String>,
    totp_code: String,
    acl_text: String,
    /// the file manager's open prompt (new folder, rename, delete)
    fm_dialog: Option<FmDialog>,
    /// per session: the whiteboard, and the last full-size picture (for screenshots)
    boards: HashMap<u32, Board>,
    last_pic: HashMap<u32, crate::video::Picture>,
    act_dialog: Option<ActDialog>,
    /// displays whose privacy cover is already kept out of the shared picture
    covered: Vec<u32>,
    /// started with --background: minimize on the first frame
    minimize_once: bool,
    /// A finger down on a remote screen (phones, see touch_gestures).
    touch: Option<Touch>,
    /// Phones: the soft keyboard is up for typing into a session (toolbar button).
    soft_keyboard: bool,
    /// Phones: the finger moves a pointer like a laptop's touchpad (AnyDesk's default);
    /// off, a finger works where it touches.
    touchpad: bool,
    /// Phones: each session's pointer (0..1 of the remote screen) and the last tap (a tap
    /// and then a press drags).
    tp_cursor: HashMap<u32, Pos2>,
    tp_last_tap: Option<Instant>,
    /// Phones: each session's pinch zoom — the scale over "fit" and where the picture starts.
    zoom: HashMap<u32, (f32, Pos2)>,
    /// Phones: the session menu (the round button) is open, and where its buttons were drawn
    /// (fingers there are not for the host).
    fab_open: bool,
    /// A Cmd/Ctrl+C/X/V press went to the remote side; its release goes too.
    shortcut_down: bool,
    /// Cmd/Ctrl+V sent this side's copied files instead (its release stays here)
    files_pasted: bool,
    fab_rects: Vec<Rect>,
    /// The other side's camera window per session (size, folded), and where the camera
    /// windows are this frame (the pointer there is not for the host).
    pips: HashMap<u32, Pip>,
    pip_rects: Vec<Rect>,
    /// Settings → startup: (starts with the system, service installed), looked up off the
    /// UI thread (Windows asks `reg` and the service manager), and when it was.
    startup: Arc<Mutex<(bool, bool)>>,
    startup_checked: Option<Instant>,
    /// update check: None = idle, Some(None) = checking, Some(Some(..)) = result text + ok
    update: std::sync::Arc<Mutex<Option<(String, Option<bool>)>>>,
}

/// What a file manager pane asks for this frame (applied after drawing).
enum FmAct {
    Open(String),
    Click(usize, bool, bool),
    SelectAll,
    SelectNone,
    Copy,
    Refresh,
    Mkdir,
    Rename,
    Delete,
    Cancel(String),
    ClearDone,
}

enum FmDialog {
    Mkdir { key: u32, remote: bool, name: String },
    Rename { key: u32, remote: bool, old: String, name: String },
    Delete { key: u32, remote: bool, names: Vec<String> },
}

const TITLEBAR_H: f32 = 52.0;
const TOOLBAR_H: f32 = 44.0;
const CHAT_W: f32 = 330.0;
/// 1:1 view: how close to the stage edge the captured pointer starts scrolling, and how fast (points/s)
const PAN_EDGE: f32 = 28.0;
const PAN_SPEED: f32 = 900.0;
/// file manager row heights: listing, transfers
const FM_ROW: f32 = 28.0;
const FM_TR_ROW: f32 = 34.0;
/// how often updates are looked for while the app runs
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);
/// an automatic update waits until nobody has used the window for this long
const UPDATE_QUIET: Duration = Duration::from_secs(120);
/// an update is being downloaded and installed
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// what the UI tells the update scheduler: a session is open, updates install by
/// themselves, the window was last used this many seconds after start, and "Download and
/// install" was pressed during a session
static UPD_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static UPD_AUTO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static UPD_INPUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UPD_NOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static UPD_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn since_start() -> Duration {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed()
}

type UpdateSlot = Arc<Mutex<Option<(String, Option<bool>)>>>;

/// Looks for updates on start and every UPDATE_EVERY, and installs a found one when no
/// session is open: at once when asked for, otherwise once nobody has used the window for
/// UPDATE_QUIET (AnyDesk installs its updates by itself, too).
async fn update_scheduler(slot: UpdateSlot) {
    use std::sync::atomic::Ordering::SeqCst;
    since_start();
    let mut checked: Option<Instant> = None;
    let mut waited: Option<Instant> = None;
    loop {
        let auto = UPD_AUTO.load(SeqCst);
        if auto && checked.is_none_or(|t| t.elapsed() > UPDATE_EVERY) {
            checked = Some(Instant::now());
            run_check(slot.clone(), false).await;
        }
        let found = UPDATE.lock().unwrap().clone();
        if let Some(u) = &found {
            let quiet = since_start().saturating_sub(Duration::from_secs(UPD_INPUT.load(SeqCst)));
            let (asked, busy) = (UPD_NOW.load(SeqCst), UPD_BUSY.load(SeqCst));
            if !INSTALLING.load(SeqCst) && !busy && (asked || (auto && quiet >= UPDATE_QUIET)) {
                eprintln!("[update] installing {}: {}", u.version, if asked { "asked for".to_string() } else { format!("no session, window unused for {}s", quiet.as_secs()) });
                install_update(u.clone(), slot.clone()).await;
            } else if auto && !INSTALLING.load(SeqCst) && waited.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                waited = Some(Instant::now());
                eprintln!("[update] {} waits: {}", u.version, if busy { "a session is open".to_string() } else { format!("window used {}s ago", quiet.as_secs()) });
            }
        }
        tokio::time::sleep(Duration::from_secs(if found.is_some() { 5 } else { 30 })).await;
    }
}

async fn run_check(slot: UpdateSlot, manual: bool) {
    if INSTALLING.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    *slot.lock().unwrap() = Some((t("start.checking"), None));
    bus::wake();
    // MANYDESK_UPDATE_CURRENT=<version>: check as that version, to try the update path
    let cur = std::env::var("MANYDESK_UPDATE_CURRENT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let msg = match crate::platform::update::check(&cur).await {
        Ok(Some(u)) => {
            let msg = (tf("start.found", &[("v", &u.version)]), Some(true));
            *UPDATE.lock().unwrap() = Some(u);
            Some(msg)
        }
        Ok(None) if manual => Some((t("start.upToDate"), Some(false))),
        Err(e) if manual => Some((tf("start.checkFailed", &[("e", &e)]), Some(false))),
        _ => None,
    };
    *slot.lock().unwrap() = msg;
    bus::wake();
}

/// Downloads the update and puts it in place; ManyDesk then quits and the new version
/// starts. When it cannot install by itself (no write access to its folder, a tarball
/// copy) it opens the installer instead.
async fn install_update(u: crate::platform::update::Update, slot: UpdateSlot) {
    use std::sync::atomic::Ordering::SeqCst;
    if INSTALLING.swap(true, SeqCst) {
        return;
    }
    UPD_NOW.store(false, SeqCst);
    let msg = tf("start.updating", &[("v", &u.version)]);
    *slot.lock().unwrap() = Some((msg.clone(), None));
    bus::emit("toast", json!({ "text": msg, "error": false }));
    match crate::platform::update::install(&u).await {
        Ok(()) => {
            // let the log and the settings reach the disk, then make way for the new copy
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::process::exit(0);
        }
        Err((e, package)) => {
            let opened = package.as_deref().map(|p| crate::platform::update::open(p).is_ok()).unwrap_or(false);
            let text = if opened { tf("start.updateByHand", &[("e", &e)]) } else { tf("start.checkFailed", &[("e", &e)]) };
            *slot.lock().unwrap() = Some((text.clone(), Some(false)));
            *UPDATE.lock().unwrap() = None;
            INSTALLING.store(false, SeqCst);
            bus::emit("toast", json!({ "text": text, "error": true }));
        }
    }
}
/// the newest update's download, after a successful check
static UPDATE: Mutex<Option<crate::platform::update::Update>> = Mutex::new(None);

/// Text in the UI font with the CSS line-height (1.45).
fn text(s: impl Into<String>, size: f32, w: W, color: Color32) -> RichText {
    RichText::new(s).font(th::font(size, w)).color(color).line_height(Some((size * 1.45).round()))
}

/// Monospace text with the CSS letter-spacing.
fn mono(s: impl Into<String>, size: f32, w: W, color: Color32, spacing: f32) -> RichText {
    RichText::new(s).font(th::mono(size, w)).color(color).extra_letter_spacing(spacing)
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rx: Receiver<Msg>) -> Self {
        bus::set_waker(cc.egui_ctx.clone());
        crate::perf::startup("app");
        th::setup(&cc.egui_ctx);
        crate::perf::startup("fonts");
        let mut core = Core::new();
        // Test hook: MANYDESK_AUTOCONNECT=<id> connects on start (headless tests), as
        // MANYDESK_AUTOMODE=files|terminal says (default: the desktop).
        if let Ok(id) = std::env::var("MANYDESK_AUTOCONNECT") {
            core.connect_mode(&id, crate::core::SessMode::parse(&std::env::var("MANYDESK_AUTOMODE").unwrap_or_default()));
        }
        let stun_text = core.store.stun.join("\n");
        let turn = core.store.turn.clone();
        let acl_text = core.store.acl.iter().map(|i| rv::format_id(i)).collect::<Vec<_>>().join("\n");
        // Test hook: MANYDESK_PAGE=share|settings opens that page (screenshots).
        let page = match std::env::var("MANYDESK_PAGE").as_deref() {
            Ok("share") => Page::Share,
            Ok("settings") => Page::Settings,
            _ => Page::Remote,
        };
        let touchpad = core.store.touchpad;
        App {
            req_perms: core.store.perms,
            req_seen: None,
            core,
            rx,
            icons: Icons::default(),
            page,
            address: String::new(),
            ac_sel: None,
            ac_closed: false,
            ac_hover: false,
            fullscreen: false,
            tb_hide_at: None,
            thumbs: HashMap::new(),
            screen_thumbs: HashMap::new(),
            frames: HashMap::new(),
            mac_local: cfg!(target_os = "macos"),
            host_chat: None,
            collapsed: HashSet::new(),
            host_live_at: HashMap::new(),
            auto_folded: HashSet::new(),
            overlay_hidden: Vec::new(),
            diag: Default::default(),
            stun_text,
            turn: turn.clone(),
            turn_urls: turn.urls.join("\n"),
            turn_out: Default::default(),
            un_pw: Default::default(),
            totp_new: None,
            totp_code: String::new(),
            acl_text,
            fm_dialog: None,
            boards: HashMap::new(),
            last_pic: HashMap::new(),
            act_dialog: None,
            covered: Vec::new(),
            minimize_once: std::env::args().any(|a| a == "--background"),
            touch: None,
            soft_keyboard: false,
            touchpad,
            tp_cursor: HashMap::new(),
            tp_last_tap: None,
            zoom: HashMap::new(),
            fab_open: false,
            shortcut_down: false,
            files_pasted: false,
            fab_rects: Vec::new(),
            pips: HashMap::new(),
            pip_rects: Vec::new(),
            startup: Default::default(),
            startup_checked: None,
            update: Default::default(),
        }
    }
}

impl eframe::App for App {
    /// Transparent background, for the host overlay window (the main window paints
    /// every pixel with its panels anyway).
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    /// Test hooks: MANYDESK_TESTCOPY=<text> copies it once the window is up (as "copy my
    /// ID" does); MANYDESK_TESTPASTE=<text>, 6 s into a session: Cmd/Ctrl+V as the keyboard
    /// gives it (Paste with the clipboard's text, then the V's release), then 1 s later the
    /// release alone (a Cmd+V with files on the clipboard).
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        static STEP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        static LIVE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        use std::sync::atomic::Ordering::SeqCst;
        if ctx.cumulative_pass_nr() == 3 {
            if let Ok(text) = std::env::var("MANYDESK_TESTCOPY") {
                ctx.copy_text(text);
            }
            // MANYDESK_TESTADDR=<text>: typed into the address bar (its suggestions show)
            if let Ok(text) = std::env::var("MANYDESK_TESTADDR") {
                self.ac_sel = default_pick(&text);
                self.address = text;
                ctx.memory_mut(|m| m.request_focus(Id::unique("rv-peer-id")));
            }
            // MANYDESK_TESTCONNECT=<id>: connects; MANYDESK_TESTACCEPT=1: accepts every request
            if let Ok(id) = std::env::var("MANYDESK_TESTCONNECT") {
                self.core.connect(&id);
            }
        }
        if self.core.pending.is_some() && std::env::var_os("MANYDESK_TESTACCEPT").is_some() {
            let perms = self.core.store.unattended.perms;
            self.core.decide(true, perms);
        }
        // MANYDESK_TESTTERM=<command>: opens the terminal, runs it, prints the screen
        if let Ok(cmd) = std::env::var("MANYDESK_TESTTERM") {
            static T0: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            static TSTEP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
            if let Some(key) = self.core.tabs.iter().find(|t| t.state == TabState::Live && t.remote.terminal).map(|t| t.key) {
                ctx.request_repaint_after(Duration::from_millis(100));
                let since = T0.lock().unwrap().get_or_insert_with(Instant::now).elapsed();
                let step = TSTEP.load(SeqCst);
                if step == 0 && since > Duration::from_secs(3) {
                    if !self.core.tabs.iter().any(|t| t.key == key && t.term_open) {
                        self.toggle_term(key);
                    }
                    TSTEP.store(1, SeqCst);
                } else if step == 1 && since > Duration::from_secs(6) {
                    if let Some(term) = self.core.tab(key).and_then(|t| t.term.as_ref()) {
                        term.send_keys(format!("{cmd}\r").as_bytes());
                    }
                    TSTEP.store(2, SeqCst);
                } else if step == 2 && since > Duration::from_secs(9) {
                    let screen = self.core.tabs.iter().find(|t| t.key == key).and_then(|t| t.term.as_ref()).map(|t| t.parser.screen().contents()).unwrap_or_default();
                    eprintln!("[testterm] screen:\n{}", screen.trim_end());
                    TSTEP.store(3, SeqCst);
                }
            }
        }
        // MANYDESK_TESTTYPE=<text>: typed into the focused field one key a frame, from 5 s
        if let Ok(text) = std::env::var("MANYDESK_TESTTYPE") {
            static TYPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            static T0: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let since = T0.lock().unwrap().get_or_insert_with(Instant::now).elapsed();
            ctx.request_repaint_after(Duration::from_millis(80));
            let n = TYPED.load(SeqCst);
            // MANYDESK_TESTCLICK=x,y: a click there first (points), as the mouse does it
            static CLICKED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
            if let Some((x, y)) = std::env::var("MANYDESK_TESTCLICK").ok().and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))) {
                let c = CLICKED.load(SeqCst);
                if since > Duration::from_secs(4) && c < 3 {
                    let pos = Pos2::new(x, y);
                    match c {
                        0 => raw.events.push(egui::Event::PointerMoved(pos)),
                        1 => raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }),
                        _ => raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }),
                    }
                    CLICKED.store(c + 1, SeqCst);
                    if c == 2 {
                        eprintln!("[testtype] clicked at {pos:?}");
                    }
                }
            }
            if since > Duration::from_secs(5) {
                if let Some(c) = text.chars().nth(n) {
                    raw.events.push(egui::Event::Text(c.to_string()));
                    TYPED.store(n + 1, SeqCst);
                    eprintln!("[testtype] typed {c:?}, focus {:?}", ctx.memory(|m| m.focused()));
                } else if n == text.chars().count() {
                    TYPED.store(n + 1, SeqCst);
                    eprintln!("[testtype] field now {:?}", self.address);
                }
            }
        }
        let Ok(text) = std::env::var("MANYDESK_TESTPASTE") else { return };
        let Some(key) = self.core.tabs.iter().find(|t| t.state == TabState::Live && t.remote.control).map(|t| t.key) else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        let since = LIVE.lock().unwrap().get_or_insert_with(Instant::now).elapsed();
        let cmd = egui::Modifiers::COMMAND;
        let up = egui::Event::Key { key: egui::Key::V, physical_key: Some(egui::Key::V), pressed: false, repeat: false, modifiers: cmd };
        let step = STEP.load(SeqCst);
        if step == 0 && since > Duration::from_secs(6) {
            self.core.set_capture(Some(key));
            raw.events.push(egui::Event::Paste(text));
            STEP.store(1, SeqCst);
        } else if step == 1 {
            raw.events.push(up);
            STEP.store(2, SeqCst);
        } else if step == 2 && since > Duration::from_secs(7) {
            raw.events.push(up);
            eprintln!("[testpaste] done");
            STEP.store(3, SeqCst);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        let _turn = background::claim(self);
        crate::perf::beat();
        crate::perf::phase("logic");
        // Test hook: MANYDESK_MINIMIZE=1 minimizes the window once it is up (hidden host).
        if ctx.cumulative_pass_nr() == 3 && std::env::var_os("MANYDESK_MINIMIZE").is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        crate::perf::phase("core.tick");
        self.tick_core();
        crate::perf::phase("logic");
        // Keep ticking while sessions exist, also when the window is hidden (then eframe
        // runs only this, not ui()).
        if !self.core.tabs.is_empty() || !self.core.hosts.is_empty() || self.core.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        crate::perf::phase("os");
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        let _turn = background::claim(self);
        let ctx = ui.ctx().clone();
        self.pip_rects.clear();
        crate::perf::tick("uiframe");
        crate::perf::phase("ui");
        crate::perf::startup("first frame");
        if self.minimize_once {
            self.minimize_once = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        self.update_tick(&ctx);
        let active = self.core.active.filter(|k| self.core.tabs.iter().any(|t| t.key == *k));
        // phones: keep clear of the status bar / notch and the navigation bar
        #[cfg(mobile)]
        {
            let (top, bottom) = crate::phone::insets();
            if top > 0.0 {
                egui::Panel::top("safe-top").exact_size(top).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |_| {});
            }
            if bottom > 0.0 {
                egui::Panel::bottom("safe-bottom").exact_size(bottom).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |_| {});
            }
        }
        let narrow = compact(&ctx);
        if narrow {
            // a phone: an app bar at home, the whole screen for a session
            if active.is_none() {
                self.app_bar(ui);
            }
        } else if !(self.fullscreen && active.is_some()) {
            self.titlebar(ui);
        }
        // A host chat is a column of its own on the right (body.chat-open).
        if let Some(h) = self.host_chat {
            if self.core.hosts.iter().any(|x| x.key == h && x.chat.open) {
                self.host_chat_column(ui, h);
            } else {
                self.host_chat = None;
            }
        }
        crate::perf::phase(if active.is_some() { "ui.session" } else { "ui.home" });
        match active {
            Some(k) => self.session_view(ui, k),
            None => self.home(ui),
        }
        crate::perf::phase("ui.overlays");
        self.request_window(&ctx);
        self.host_stack(&ctx);
        self.host_cameras(&ctx);
        self.pointer_overlay(&ctx);
        self.privacy_cover(&ctx);
        self.act_dialog(&ctx);
        self.toasts(&ctx);
        #[cfg(mobile)]
        crate::phone::sync_keyboard(ctx.egui_wants_keyboard_input() || (self.soft_keyboard && active.is_some()));
        crate::perf::phase("os");
    }
}

impl App {
    /// The core's turn: events in, sessions forward (also from android's background tick).
    fn tick_core(&mut self) {
        self.core.tick(&self.rx);
        if std::env::var_os("MANYDESK_TESTGRANTTERM").is_some() {
            if let Some(h) = self.core.hosts.iter().find(|h| h.state == crate::core::HostState::Live && !h.perms.terminal && h.live_for() > Duration::from_secs(8)) {
                let (k, p) = (h.key, crate::store::Permissions { terminal: true, ..h.perms });
                eprintln!("[test] host ticks Terminal");
                self.core.set_host_perms(k, p);
            }
        }
        // Test hook: accept requests even while the window is hidden (no ui pass then).
        if self.core.pending.is_some() && std::env::var_os("MANYDESK_AUTOACCEPT").is_some() {
            // tests may ask for every permission (restart, privacy, tunnel included)
            let mut perms = if std::env::var_os("MANYDESK_AUTOFULL").is_some() { crate::store::Permissions::FULL } else { self.req_perms };
            // MANYDESK_TESTGRANTTERM=1: Terminal is off at first and ticked 8 s into the session
            if std::env::var_os("MANYDESK_TESTGRANTTERM").is_some() {
                perms.terminal = false;
            }
            self.core.decide(true, perms);
        }
        self.core.apply_isolation();
        #[cfg(target_os = "android")]
        crate::android::notify_request(self.core.pending.as_ref().map(|r| r.name.as_str()));
    }

    // ================================================================ title bar ===

    fn titlebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("titlebar").exact_size(TITLEBAR_H).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
            let bar = ui.max_rect();
            // The empty title bar moves the window (data-tauri-drag-region).
            let drag = ui.interact(bar, Id::unique("titlebar-drag"), Sense::click_and_drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, th::BORDER));

            // tabs
            let mut x = bar.left() + if cfg!(target_os = "macos") { 84.0 } else { 12.0 };
            let home_active = self.core.active.is_none();
            let (r, _, clicked) = self.apptab(ui, x, bar, home_active, None);
            if clicked {
                self.core.active = None;
                self.core.set_capture(None);
            }
            x = r.right() + 2.0;
            let tabs: Vec<(u32, bool)> = self.core.tabs.iter().map(|t| (t.key, self.core.active == Some(t.key))).collect();
            let mut close = None;
            for (key, on) in tabs {
                let (r, x_clicked, clicked) = self.apptab(ui, x, bar, on, Some(key));
                if x_clicked {
                    close = Some(key);
                } else if clicked {
                    self.core.active = Some(key);
                }
                x = r.right() + 2.0;
            }
            if let Some(k) = close {
                self.core.close_tab(k);
            }

            // address bar, centred in the space that is left
            let right = bar.right() - 12.0 - 32.0 - 14.0;
            let left = x + 14.0;
            let w = (right - left).clamp(0.0, 460.0);
            if w > 160.0 {
                let ab = Rect::from_min_size(Pos2::new(left + (right - left - w) / 2.0, bar.center().y - 17.0), Vec2::new(w, 34.0));
                self.address_bar(ui, ab);
            }
            let gear = Rect::from_min_size(Pos2::new(bar.right() - 12.0 - 32.0, bar.center().y - 16.0), Vec2::splat(32.0));
            ui.scope_builder(UiBuilder::new().max_rect(gear), |ui| {
                if th::iconbtn(ui, &self.icons, "gear", &t("tt.settings")).clicked() {
                    self.core.active = None;
                    self.page = Page::Settings;
                }
            });
        });
    }

    /// One .apptab; returns (rect, close clicked, tab clicked).
    fn apptab(&mut self, ui: &mut Ui, x: f32, bar: Rect, active: bool, key: Option<u32>) -> (Rect, bool, bool) {
        let tab = key.and_then(|k| self.core.tabs.iter().find(|t| t.key == k));
        let label = match tab {
            Some(t) => t.label.clone(),
            None => t("home"),
        };
        let fnt = th::font(13.5, if active { W::Medium } else { W::Regular });
        let label_g = ui.painter().layout_no_wrap(label.clone(), fnt.clone(), th::TEXT);
        let label_w = label_g.size().x.min(150.0);
        let id_text = tab.map(|t| format!("· {}", &t.peer_id[t.peer_id.len().saturating_sub(3)..])).unwrap_or_default();
        let id_g = ui.painter().layout_no_wrap(id_text, th::mono(11.5, W::Regular), th::MUTED);
        let unread = tab.map(|t| t.chat.unread).unwrap_or(0);
        let mut w = 14.0 + 18.0 + 7.0 + label_w + 14.0;
        if tab.is_some() {
            w += 7.0 + 7.0 + id_g.size().x + 7.0 + 18.0 + 2.0;
            if unread > 0 {
                w += 7.0 + 18.0;
            }
        }
        let rect = Rect::from_min_max(Pos2::new(x, bar.top() + 10.0), Pos2::new(x + w, bar.bottom()));
        let resp = ui.interact(rect, Id::unique(("apptab", key.unwrap_or(0))), Sense::click());
        let hover = resp.hovered();
        let p = ui.painter();
        if active {
            p.rect(rect.translate(Vec2::new(0.0, 1.0)), CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, th::BG, Stroke::new(1.0, th::BORDER), StrokeKind::Inside);
            p.hline(rect.left() + 1.0..=rect.right() - 1.0, bar.bottom() - 0.5, Stroke::new(1.5, th::BG));
        }
        let fg = if active || hover { th::TEXT } else { th::MUTED };
        let mut cx = rect.left() + 14.0;
        let cy = rect.center().y + 1.0;
        if let Some(t) = tab {
            let c = match t.state {
                TabState::Live => th::OK,
                TabState::Failed(_) => th::DANGER,
                _ => th::WARN,
            };
            p.circle_filled(Pos2::new(cx + 3.5, cy), 3.5, c);
            cx += 7.0 + 7.0;
        }
        self.icons.paint(ui, tab.map_or("home", |t| mode_icon(t.mode)), Rect::from_center_size(Pos2::new(cx + 9.0, cy), Vec2::splat(18.0)), fg);
        cx += 18.0 + 7.0;
        let g = ui.painter().layout_no_wrap(label, fnt, fg);
        let clip = Rect::from_min_size(Pos2::new(cx, rect.top()), Vec2::new(label_w, rect.height()));
        ui.painter().with_clip_rect(clip).galley(Pos2::new(cx, cy - g.size().y / 2.0), g, fg);
        cx += label_w;
        let mut x_clicked = false;
        if tab.is_some() {
            cx += 7.0;
            ui.painter().galley(Pos2::new(cx, cy - id_g.size().y / 2.0), id_g.clone(), th::MUTED);
            cx += id_g.size().x + 7.0;
            if unread > 0 {
                let b = Rect::from_min_size(Pos2::new(cx, cy - 8.0), Vec2::new(18.0, 16.0));
                th::badge(ui, b, unread);
                cx += 18.0 + 7.0;
            }
            let xr = Rect::from_center_size(Pos2::new(cx + 9.0, cy), Vec2::splat(18.0));
            let xresp = ui.interact(xr, Id::unique(("tab-x", key)), Sense::click()).on_hover_text(t("tt.closeTab"));
            if xresp.hovered() {
                ui.painter().rect_filled(xr, 4.0, th::BORDER);
            }
            self.icons.paint(ui, "x", Rect::from_center_size(xr.center(), Vec2::splat(12.0)), fg);
            x_clicked = xresp.clicked();
        }
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        (rect, x_clicked, resp.clicked() && !x_clicked)
    }

    fn address_bar(&mut self, ui: &mut Ui, ab: Rect) {
        let id = Id::unique("rv-peer-id");
        let focused = ui.ctx().memory(|m| m.has_focus(id));
        let p = ui.painter();
        if focused {
            p.rect_stroke(ab.expand(3.0), 9.0, Stroke::new(3.0, th::ACCENT_SOFT), StrokeKind::Inside);
        }
        p.rect(ab, 6.0, if focused { th::SURFACE } else { th::SURFACE_2 }, Stroke::new(1.0, if focused { th::ACCENT } else { th::BORDER_STRONG }), StrokeKind::Inside);
        self.icons.paint(ui, "monitor", Rect::from_min_size(Pos2::new(ab.left() + 10.0, ab.center().y - 9.0), Vec2::splat(18.0)), th::MUTED);
        let go = Rect::from_min_size(Pos2::new(ab.right() - 4.0 - 30.0, ab.center().y - 13.0), Vec2::new(30.0, 26.0));
        // AnyDesk's "Browse files" beside Connect (and RustDesk's terminal): no desktop
        let term_b = go.translate(Vec2::new(-30.0 - 4.0, 0.0));
        let files_b = term_b.translate(Vec2::new(-30.0 - 2.0, 0.0));
        let input = Rect::from_min_max(Pos2::new(ab.left() + 10.0 + 18.0 + 8.0, ab.top() + 4.0), Pos2::new(files_b.left() - 6.0, ab.bottom() - 4.0));
        let mut submit = false;
        ui.scope_builder(UiBuilder::new().max_rect(input), |ui| {
            // No length limit: pasted text like "ID: 123 456 789" is fine, only digits count.
            let mut out = egui::TextEdit::singleline(&mut self.address)
                .id(id)
                .frame(egui::Frame::NONE)
                .font(th::mono(14.0, W::Semibold))
                .text_color(th::TEXT)
                .hint_text(RichText::new(t("addr.ph")).font(th::font(13.5, W::Regular)).color(th::MUTED))
                .desired_width(input.width())
                .min_size(input.size())
                .align(egui::Align2::LEFT_CENTER)
                .show(ui);
            if out.response.changed() {
                self.ac_closed = false;
                self.ac_sel = default_pick(&self.address);
            }
            // an IP address (direct connection on the LAN or a VPN) and a name being looked
            // up in the list stay as typed
            if out.response.changed() && !self.address.contains(['.', ':']) && !is_name_query(&self.address) {
                // digits only, grouped 3-3-3 like the web field; the cursor stays after the
                // same digit it was after (otherwise typing in the middle scrambles the order)
                let cursor: usize = out.cursor_range.map(|r| r.primary.index.into()).unwrap_or(usize::MAX);
                let (text, at) = reformat_id(&self.address, cursor);
                self.address = text;
                out.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
                out.state.store(ui.ctx(), id);
            }
            if out.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
        });
        let picked = self.address_suggestions(ui, ab, focused);
        let gr = ui.interact(go, Id::unique("rv-connect"), Sense::click()).on_hover_text(t("tt.connect"));
        ui.painter().rect_filled(go, 4.0, if gr.hovered() { th::ACCENT_HOVER } else { th::ACCENT });
        self.icons.paint(ui, "arrow", Rect::from_center_size(go.center(), Vec2::splat(18.0)), Color32::WHITE);
        if gr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let mut mode = (gr.clicked() || submit).then_some(crate::core::SessMode::Desktop);
        for (r, icon, tip, m) in [(files_b, "file", "tt.browse", crate::core::SessMode::Files), (term_b, "terminal", "tt.termOnly", crate::core::SessMode::Terminal)] {
            let resp = ui.interact(r, Id::unique(("rv-mode", icon)), Sense::click()).on_hover_text(t(tip));
            if resp.hovered() {
                ui.painter().rect_filled(r, 4.0, th::SURFACE_2);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            self.icons.paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(17.0)), if resp.hovered() { th::ACCENT } else { th::MUTED });
            if resp.clicked() {
                mode = Some(m);
            }
        }
        // a suggestion clicked connects; with one highlighted, Enter and the buttons take it
        let target = match (picked, self.ac_sel) {
            (Some(id), _) => {
                mode.get_or_insert(crate::core::SessMode::Desktop);
                Some(id)
            }
            (None, Some(i)) if !self.ac_closed => suggestions(&self.core, &self.address).get(i).map(|s| s.id.clone()),
            _ => None,
        };
        if let Some(m) = mode.filter(|_| !self.address.trim().is_empty() || target.is_some()) {
            let a = std::mem::take(&mut self.address);
            self.ac_sel = None;
            self.core.connect_mode(&target.unwrap_or(a), m);
        }
    }

    /// The list under the address bar while typing: recent and LAN computers that match.
    /// Returns the ID clicked.
    fn address_suggestions(&mut self, ui: &mut Ui, ab: Rect, focused: bool) -> Option<String> {
        let list = suggestions(&self.core, &self.address);
        if list.is_empty() || self.ac_closed || !(focused || self.ac_hover) {
            self.ac_hover = false;
            return None;
        }
        // ↑ ↓ move, Esc closes (the text field keeps the keys it uses itself)
        if focused {
            let (down, up, esc) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::Escape)));
            let n = list.len();
            if down {
                self.ac_sel = Some(self.ac_sel.map_or(0, |s| (s + 1).min(n - 1)));
            }
            if up {
                self.ac_sel = self.ac_sel.and_then(|s| s.checked_sub(1));
            }
            if esc {
                self.ac_closed = true;
                return None;
            }
        }
        if self.ac_sel.is_some_and(|s| s >= list.len()) {
            self.ac_sel = Some(list.len() - 1);
        }
        let mut picked = None;
        let resp = egui::Area::new(Id::unique("rv-suggest")).order(egui::Order::Foreground).fixed_pos(ab.left_bottom() + Vec2::new(0.0, 6.0)).show(ui.ctx(), |ui| {
            egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(8).shadow(th::shadow_lg()).inner_margin(Margin::same(4)).show(ui, |ui| {
                ui.set_width(ab.width() - 8.0);
                for (n, s) in list.iter().enumerate() {
                    let (r, row) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
                    let hot = row.hovered() || self.ac_sel == Some(n);
                    if hot {
                        ui.painter().rect_filled(r, 6.0, if self.ac_sel == Some(n) { th::ACCENT_SOFT } else { th::SURFACE_2 });
                    }
                    if row.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let state = self.core.presence_of(&s.id);
                    self.icons.paint(ui, "monitor", Rect::from_center_size(Pos2::new(r.left() + 20.0, r.center().y), Vec2::splat(18.0)), if s.lan || state == "online" { th::ACCENT } else { th::MUTED });
                    if s.lan || state == "online" {
                        ui.painter().circle_filled(Pos2::new(r.left() + 27.0, r.center().y + 6.0), 3.5, th::OK);
                    }
                    let name = if s.name.is_empty() { rv::format_id(&s.id) } else { s.name.clone() };
                    let tag = if s.lan { t("lan.title") } else { t("recent.title") };
                    let tg = ui.painter().layout_no_wrap(tag, th::font(11.0, W::Medium), th::MUTED);
                    let mut job = egui::text::LayoutJob::simple_singleline(name, th::font(13.5, W::Semibold), th::TEXT);
                    job.wrap = egui::text::TextWrapping::truncate_at_width((r.width() - 40.0 - tg.size().x - 24.0).max(60.0));
                    let ng = ui.painter().layout_job(job);
                    ui.painter().galley(Pos2::new(r.left() + 40.0, r.center().y - 1.0 - ng.size().y), ng, th::TEXT);
                    let sub = if s.ip.is_empty() { rv::format_id(&s.id) } else { format!("{} · {}", rv::format_id(&s.id), s.ip) };
                    ui.painter().text(Pos2::new(r.left() + 40.0, r.center().y + 2.0), Align2::LEFT_TOP, sub, th::mono(11.5, W::Regular), th::MUTED);
                    ui.painter().galley(Pos2::new(r.right() - 12.0 - tg.size().x, r.center().y - tg.size().y / 2.0), tg, th::MUTED);
                    if row.clicked() {
                        picked = Some(s.id.clone());
                    }
                }
            });
        });
        self.ac_hover = ui.rect_contains_pointer(resp.response.rect);
        if picked.is_some() {
            self.ac_hover = false;
        }
        picked
    }

    // ================================================================ home ===

    fn home(&mut self, ui: &mut Ui) {
        // A phone (or a narrow window): one column under the app bar, AnyDesk's way.
        if compact(ui.ctx()) {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(th::BG)).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    egui::Frame::new().inner_margin(Margin { left: 14, right: 14, top: 14, bottom: 32 }).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match self.page {
                            Page::Remote => self.mobile_home(ui),
                            Page::Share => self.page_share(ui),
                            Page::Settings => self.page_settings(ui),
                        }
                    });
                });
            });
            return;
        }
        egui::Panel::left("sidebar").exact_size(280.0).resizable(false).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().vline(r.right() - 0.5, r.y_range(), Stroke::new(1.0, th::BORDER));
            self.sidebar(ui);
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(th::BG)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let w = (ui.available_width() - 56.0).clamp(200.0, 980.0);
                egui::Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 40 }).show(ui, |ui| {
                    ui.set_width(w);
                    match self.page {
                        Page::Remote => self.page_remote(ui),
                        Page::Share => self.page_share(ui),
                        Page::Settings => self.page_settings(ui),
                    }
                });
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        // .this-desk
        egui::Frame::new().inner_margin(Margin { left: 18, right: 18, top: 20, bottom: 12 }).show(ui, |ui| {
            ui.set_width(full.width() - 36.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(text(t("desk.title"), 15.0, W::Semibold, th::TEXT));
            ui.add_space(2.0);
            ui.label(text(t("desk.sub"), 12.0, W::Regular, th::MUTED));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(mono(rv::format_id(&self.core.store.my_id), 25.0, W::Semibold, th::ACCENT, 1.5));
                if th::iconbtn(ui, &self.icons, "copy", &t("tt.copy")).clicked() {
                    ui.ctx().copy_text(self.core.store.my_id.clone());
                    self.core.toast(t("toast.copied"), false);
                }
            });
            ui.add_space(8.0);
            let (n, total) = self.core.rv_status;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                if n > 0 {
                    th::dot(ui, th::OK);
                    ui.label(text(tf("rv.ready", &[("n", &n.to_string()), ("total", &total.to_string())]), 12.0, W::Regular, th::MUTED));
                } else {
                    th::dot(ui, th::WARN);
                    ui.label(text(t("rv.connecting"), 12.0, W::Regular, th::MUTED));
                }
            });
        });
        let y = ui.cursor().top();
        ui.painter().hline(full.x_range(), y, Stroke::new(1.0, th::BORDER));
        // .sidenav
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        for (page, icon, key) in [(Page::Remote, "monitor", "nav.remote"), (Page::Share, "share", "nav.share"), (Page::Settings, "gear", "nav.settings")] {
            let on = self.page == page;
            let r = Rect::from_min_size(Pos2::new(full.left() + 8.0, ui.cursor().top()), Vec2::new(full.width() - 16.0, 38.0));
            let resp = ui.allocate_rect(r, Sense::click());
            if on {
                ui.painter().rect_filled(r, 6.0, th::ACCENT_SOFT);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 6.0, th::SURFACE_2);
            }
            self.icons.paint(ui, icon, Rect::from_min_size(Pos2::new(r.left() + 12.0, r.center().y - 9.0), Vec2::splat(18.0)), if on { th::ACCENT } else { th::MUTED });
            let col = if on { th::ACCENT } else { th::TEXT };
            let g = ui.painter().layout_no_wrap(t(key), th::font(13.5, if on { W::Medium } else { W::Regular }), col);
            ui.painter().galley(Pos2::new(r.left() + 40.0, r.center().y - g.size().y / 2.0), g, col);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.page = page;
            }
            ui.add_space(2.0);
        }
        // .sidebar-foot
        let foot = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - 62.0), full.max);
        ui.painter().hline(foot.x_range(), foot.top(), Stroke::new(1.0, th::BORDER));
        self.icons.paint(ui, "shield", Rect::from_min_size(Pos2::new(foot.left() + 18.0, foot.top() + 14.0), Vec2::splat(15.0)), th::OK);
        let p = ui.painter();
        let g = p.layout_no_wrap(t("foot.secure"), th::font(11.5, W::Regular), th::MUTED);
        p.galley(Pos2::new(foot.left() + 18.0 + 15.0 + 7.0, foot.top() + 21.5 - g.size().y / 2.0), g, th::MUTED);
        let faint = th::mix(th::MUTED, th::SURFACE, 0.8);
        let g = p.layout_no_wrap(format!("ManyDesk {}", crate::version()), th::font(11.5, W::Regular), faint);
        p.galley(Pos2::new(foot.left() + 18.0 + 22.0, foot.top() + 38.0), g, faint);
    }

    /// Phones: the app bar — ManyDesk and settings at home; a way back and the page's name
    /// elsewhere (AnyDesk's app has the same).
    fn app_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("app-bar").exact_size(56.0).frame(egui::Frame::new().fill(th::SURFACE).inner_margin(Margin::symmetric(8, 0))).show(ui, |ui| {
            let r = ui.max_rect().expand2(Vec2::new(8.0, 0.0));
            ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, th::BORDER));
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if self.page == Page::Remote {
                    ui.add_space(6.0);
                    let (lr, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                    ui.painter().rect_filled(lr, 8.0, th::ACCENT);
                    self.icons.paint(ui, "monitor", lr.shrink(6.0), Color32::WHITE);
                    ui.label(text("ManyDesk", 18.0, W::Semibold, th::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if th::tbbtn(ui, &self.icons, "gear", false, false, &t("tt.settings")).clicked() {
                            self.page = Page::Settings;
                        }
                        if th::tbbtn(ui, &self.icons, "share", false, false, &t("nav.share")).clicked() {
                            self.page = Page::Share;
                        }
                    });
                } else {
                    if th::tbbtn(ui, &self.icons, "back", false, false, &t("home")).clicked() {
                        self.page = Page::Remote;
                    }
                    let title = if self.page == Page::Share { t("nav.share") } else { t("nav.settings") };
                    ui.label(text(title, 17.0, W::Semibold, th::TEXT));
                }
            });
        });
    }

    /// Phones: this device's address, the remote address to connect to, the sessions open
    /// now and the recent ones.
    fn mobile_home(&mut self, ui: &mut Ui) {
        let card = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
        };
        // this device
        let (n, _) = self.core.rv_status;
        let id = self.core.store.my_id.clone();
        let mut copy = false;
        card(ui, &mut |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.label(text(t("desk.title"), 13.0, W::Semibold, th::MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(text(if n > 0 { t("mob.online") } else { t("rv.connecting") }, 12.0, W::Regular, th::MUTED));
                    th::dot(ui, if n > 0 { th::OK } else { th::WARN });
                });
            });
            ui.label(text(t("desk.sub"), 12.0, W::Regular, th::MUTED));
            ui.horizontal(|ui| {
                ui.label(mono(rv::format_id(&id), 28.0, W::Semibold, th::ACCENT, 1.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if th::tbbtn(ui, &self.icons, "copy", false, false, &t("tt.copy")).clicked() {
                        copy = true;
                    }
                });
            });
        });
        if copy {
            ui.ctx().copy_text(id);
            self.core.toast(t("toast.copied"), false);
        }
        // Android: nobody can control this phone until its accessibility service is on
        if cfg!(target_os = "android") && !self.core.perms.input {
            ui.add_space(12.0);
            egui::Frame::new()
                .fill(Color32::from_rgb(0xff, 0xf8, 0xe6))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0xf3, 0xd8, 0x92)))
                .corner_radius(12)
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        self.icons.show(ui, "shield", 18.0, th::WARN);
                        ui.label(text(t("mob.axOff"), 14.0, W::Semibold, Color32::from_rgb(0x6b, 0x4e, 0x00)));
                    });
                    ui.add_space(4.0);
                    ui.label(text(t("mob.axOffHint"), 12.5, W::Regular, Color32::from_rgb(0x6b, 0x4e, 0x00)));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let bw = (ui.available_width() - 8.0) / 2.0;
                        if th::btn_sized(ui, &t("mob.axOn"), Btn::Primary, Some(bw)).clicked() {
                            crate::platform::perms::request(false);
                        }
                        if th::btn_sized(ui, &t("mob.appInfo"), Btn::Normal, Some(bw)).clicked() {
                            #[cfg(target_os = "android")]
                            crate::android::open_app_info();
                        }
                    });
                });
        }
        ui.add_space(12.0);
        // the remote device
        card(ui, &mut |ui| {
            ui.label(text(t("nav.remote"), 13.0, W::Semibold, th::MUTED));
            ui.add_space(8.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
            self.address_bar(ui, r);
        });
        // sessions open now (the tabs of a desktop)
        let open: Vec<(u32, String, String, bool)> = self.core.tabs.iter().map(|t| (t.key, t.label.clone(), t.peer_id.clone(), t.state == TabState::Live)).collect();
        if !open.is_empty() {
            ui.add_space(18.0);
            self.page_title(ui, "mob.open");
            for (k, label, peer, live) in open {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 56.0), Sense::click());
                ui.painter().rect(r, 12.0, th::SURFACE, Stroke::new(1.0, th::BORDER), StrokeKind::Inside);
                self.icons.paint(ui, "monitor", Rect::from_center_size(Pos2::new(r.left() + 24.0, r.center().y), Vec2::splat(22.0)), th::ACCENT);
                ui.painter().text(Pos2::new(r.left() + 48.0, r.center().y - 2.0), Align2::LEFT_BOTTOM, label, th::font(14.5, W::Semibold), th::TEXT);
                ui.painter().text(Pos2::new(r.left() + 48.0, r.center().y + 2.0), Align2::LEFT_TOP, rv::format_id(&peer), th::mono(12.0, W::Regular), th::MUTED);
                ui.painter().circle_filled(Pos2::new(r.right() - 20.0, r.center().y), 5.0, if live { th::OK } else { th::WARN });
                if resp.clicked() {
                    self.core.active = Some(k);
                }
                ui.add_space(8.0);
            }
        }
        ui.add_space(18.0);
        self.page_title(ui, "recent.title");
        self.recent_grid(ui);
        self.lan_list(ui);
    }

    fn page_title(&self, ui: &mut Ui, key: &str) {
        ui.label(text(t(key), 19.0, W::Semibold, th::TEXT));
        ui.add_space(8.0);
    }

    fn page_remote(&mut self, ui: &mut Ui) {
        if cfg!(target_os = "macos") && !(self.core.perms.screen && self.core.perms.input) {
            egui::Frame::new()
                .fill(Color32::from_rgb(0xff, 0xf8, 0xe6))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0xf3, 0xd8, 0x92)))
                .corner_radius(th::RADIUS)
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        self.icons.show(ui, "shield", 18.0, th::WARN);
                        ui.label(text(t("perm.banner"), 13.5, W::Regular, Color32::from_rgb(0x6b, 0x4e, 0x00)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if th::btn(ui, &t("perm.fix"), Btn::Normal).clicked() {
                                self.page = Page::Share;
                            }
                        });
                    });
                });
            ui.add_space(22.0);
        }
        self.page_title(ui, "recent.title");
        self.recent_grid(ui);
        self.lan_list(ui);
    }

    /// ManyDesks heard on the local network (AnyDesk's Discovery): a click connects.
    fn lan_list(&mut self, ui: &mut Ui) {
        let peers = crate::lan::peers();
        if peers.is_empty() {
            return;
        }
        ui.add_space(22.0);
        self.page_title(ui, "lan.title");
        let mut connect = None;
        for p in peers {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
            let hover = resp.hovered();
            ui.painter().rect(r, th::RADIUS, if hover { th::SURFACE_2 } else { th::SURFACE }, Stroke::new(1.0, if hover { th::ACCENT } else { th::BORDER }), StrokeKind::Inside);
            self.icons.paint(ui, "monitor", Rect::from_center_size(Pos2::new(r.left() + 24.0, r.center().y), Vec2::splat(20.0)), th::ACCENT);
            let name = if p.name.is_empty() { rv::format_id(&p.id) } else { p.name.clone() };
            ui.painter().text(Pos2::new(r.left() + 46.0, r.center().y - 1.0), Align2::LEFT_BOTTOM, name, th::font(13.5, W::Semibold), th::TEXT);
            let os = crate::core::os_name(&p.os);
            let sub = format!("{} · {}{}", rv::format_id(&p.id), p.ip, if os.is_empty() { String::new() } else { format!(" · {os}") });
            ui.painter().text(Pos2::new(r.left() + 46.0, r.center().y + 2.0), Align2::LEFT_TOP, sub, th::mono(11.5, W::Regular), th::MUTED);
            ui.painter().circle_filled(Pos2::new(r.right() - 48.0, r.center().y), 4.5, th::OK);
            if hover {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let mr = Rect::from_center_size(Pos2::new(r.right() - 20.0, r.center().y), Vec2::new(28.0, 26.0));
            let mresp = ui.interact(mr, Id::unique(("lan-more", &p.id)), Sense::click()).on_hover_text(t("tt.more"));
            if mresp.hovered() {
                ui.painter().rect_filled(mr, 6.0, th::BG);
            }
            self.icons.paint(ui, "more", Rect::from_center_size(mr.center(), Vec2::splat(16.0)), if mresp.hovered() { th::TEXT } else { th::MUTED });
            let resp = resp.on_hover_text(t("tt.connect"));
            if resp.clicked() && !ui.rect_contains_pointer(mr) {
                connect = Some((p.id.clone(), crate::core::SessMode::Desktop));
            }
            let mut row_menu = |ui: &mut Ui| {
                if let Some(Some(m)) = self.session_menu(ui, false) {
                    connect = Some((p.id.clone(), m));
                }
            };
            egui::Popup::menu(&mresp).show(&mut row_menu);
            resp.context_menu(&mut row_menu);
            ui.add_space(8.0);
        }
        if let Some((id, m)) = connect {
            self.core.connect_mode(&id, m);
        }
    }

    /// Connect / File transfer / Terminal (and Wake-on-LAN): the kinds of session a recent or
    /// LAN computer offers. Some(None): wake it.
    fn session_menu(&self, ui: &mut Ui, can_wake: bool) -> Option<Option<crate::core::SessMode>> {
        use crate::core::SessMode as M;
        let mut pick = None;
        ui.set_min_width(190.0);
        for (icon, key, m) in [("monitor", "ctx.connect", M::Desktop), ("file", "mode.files", M::Files), ("terminal", "mode.terminal", M::Terminal)] {
            let r = ui.horizontal(|ui| {
                self.icons.show(ui, icon, 15.0, th::MUTED);
                ui.add(egui::Button::new(t(key)).frame(false))
            });
            if r.inner.clicked() {
                pick = Some(Some(m));
                ui.close();
            }
        }
        if can_wake {
            ui.separator();
            let r = ui.horizontal(|ui| {
                self.icons.show(ui, "power", 15.0, th::MUTED);
                ui.add(egui::Button::new(t("lan.wake")).frame(false))
            });
            if r.inner.clicked() {
                pick = Some(None);
                ui.close();
            }
        }
        pick
    }

    fn recent_grid(&mut self, ui: &mut Ui) {
        let recents = self.core.store.recent.clone();
        let width = ui.available_width();
        if recents.is_empty() {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 190.0), Sense::hover());
            th::dashed_rect(ui, rect, th::BORDER_STRONG);
            let c = rect.center();
            self.icons.paint(ui, "monitor", Rect::from_center_size(Pos2::new(c.x, rect.top() + 56.0), Vec2::splat(44.0)), th::MUTED);
            let p = ui.painter();
            p.text(Pos2::new(c.x, rect.top() + 100.0), Align2::CENTER_CENTER, t("recent.empty"), th::font(14.0, W::Semibold), th::TEXT);
            p.text(Pos2::new(c.x, rect.top() + 124.0), Align2::CENTER_CENTER, t("recent.emptyHint"), th::font(13.5, W::Regular), th::MUTED);
            return;
        }
        // a phone: two tiles a row, like AnyDesk's
        let (gap, min_w) = if compact(ui.ctx()) { (10.0, 140.0) } else { (16.0, 210.0) };
        let cols = (((width + gap) / (min_w + gap)).floor() as usize).max(1);
        let tw = (width - gap * (cols as f32 - 1.0)) / cols as f32;
        let th_h = tw * 10.0 / 16.0;
        let tile_h = th_h + 58.0;
        let rows = recents.len().div_ceil(cols);
        let (area, _) = ui.allocate_exact_size(Vec2::new(width, rows as f32 * (tile_h + gap) - gap), Sense::hover());
        let mut connect = None;
        let mut forget = None;
        let mut wake = None;
        let mut menu_pick: Option<(String, Option<crate::core::SessMode>)> = None;
        for (i, r) in recents.iter().enumerate() {
            let (cx, cy) = ((i % cols) as f32, (i / cols) as f32);
            let rect = Rect::from_min_size(area.min + Vec2::new(cx * (tw + gap), cy * (tile_h + gap)), Vec2::new(tw, tile_h));
            let resp = ui.interact(rect, Id::unique(("tile", &r.id)), Sense::click()).on_hover_text(format!("{} · {}", t("tt.connect"), rv::format_id(&r.id)));
            let hover = resp.hovered();
            let state = self.core.presence_of(&r.id);
            let p = ui.painter();
            p.add(th::shadow().as_shape(rect, CornerRadius::same(th::RADIUS)));
            if hover {
                p.rect_stroke(rect.expand(3.0), 11.0, Stroke::new(3.0, th::ACCENT_SOFT), StrokeKind::Inside);
            }
            p.rect_filled(rect, th::RADIUS, th::SURFACE);
            // thumbnail (background-size: cover)
            let thumb = Rect::from_min_size(rect.min, Vec2::new(tw, th_h));
            let tr = CornerRadius { nw: th::RADIUS, ne: th::RADIUS, sw: 0, se: 0 };
            p.rect_filled(thumb, tr, th::TOAST);
            if let Some((tex, gray)) = self.thumb(ui.ctx(), &r.id, &r.thumb) {
                let tex = if state == "offline" { gray } else { tex };
                let s = tex.size_vec2();
                let (ia, ta) = (s.x / s.y, thumb.width() / thumb.height());
                let uv = if ia > ta {
                    let w = ta / ia;
                    Rect::from_min_max(Pos2::new((1.0 - w) / 2.0, 0.0), Pos2::new((1.0 + w) / 2.0, 1.0))
                } else {
                    let h = ia / ta;
                    Rect::from_min_max(Pos2::new(0.0, (1.0 - h) / 2.0), Pos2::new(1.0, (1.0 + h) / 2.0))
                };
                let mut mesh = egui::Mesh::with_texture(tex.id());
                mesh.add_rect_with_uv(thumb, uv, Color32::WHITE);
                ui.painter().with_clip_rect(thumb).add(egui::Shape::mesh(mesh));
            } else {
                self.icons.paint(ui, "monitor", Rect::from_center_size(thumb.center(), Vec2::splat(40.0)), Color32::from_rgb(0x8d, 0x93, 0x9e));
            }
            // .presence pill
            let (label, col) = match state {
                "online" => (t("pr.online"), th::OK),
                "busy" => (t("pr.busy"), th::WARN),
                "checking" => (t("pr.checking"), th::MUTED),
                _ => (t("pr.offline"), th::MUTED),
            };
            let p = ui.painter();
            let g = p.layout_no_wrap(label, th::font(11.5, W::Semibold), col);
            let pill = Rect::from_min_size(thumb.min + Vec2::new(8.0, 8.0), Vec2::new(7.0 + 8.0 + 6.0 + g.size().x + 9.0, 22.0));
            p.add(egui::epaint::Shadow { offset: [0, 1], blur: 3, spread: 0, color: Color32::from_black_alpha(64) }.as_shape(pill, CornerRadius::same(11)));
            p.rect_filled(pill, 11.0, Color32::from_white_alpha(240));
            let dc = Pos2::new(pill.left() + 7.0 + 4.0, pill.center().y);
            match state {
                "online" => {
                    p.circle_filled(dc, 7.0, th::mix(th::OK, Color32::WHITE, 0.2));
                    p.circle_filled(dc, 4.0, th::OK);
                }
                "busy" => {
                    p.circle_filled(dc, 4.0, th::WARN);
                }
                "checking" => {
                    p.circle_stroke(dc, 3.0, Stroke::new(2.0, Color32::from_rgb(0xc3, 0xc7, 0xce)));
                    let a0 = (ui.input(|i| i.time) % 0.9) as f32 / 0.9 * std::f32::consts::TAU;
                    let pts: Vec<Pos2> = (0..=6).map(|k| dc + Vec2::angled(a0 + k as f32 * 0.26) * 3.0).collect();
                    p.add(egui::Shape::line(pts, Stroke::new(2.0, th::ACCENT)));
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                }
                _ => {
                    p.circle_filled(dc, 4.0, Color32::from_rgb(0x9a, 0xa0, 0xa9));
                }
            }
            p.galley(Pos2::new(dc.x + 4.0 + 6.0, pill.center().y - g.size().y / 2.0), g, col);
            // .tile-meta
            let meta = Rect::from_min_max(Pos2::new(rect.left() + 12.0, thumb.bottom() + 10.0), Pos2::new(rect.right() - 12.0, rect.bottom() - 10.0));
            // the ⋯ has a column of its own at the right, across both lines
            let mr = Rect::from_min_max(Pos2::new(meta.right() - 26.0, meta.top() + 2.0), Pos2::new(meta.right() + 4.0, meta.bottom() - 2.0));
            let text_right = mr.left() - 6.0;
            let tg = p.layout_no_wrap(time_ago(r.ts), th::font(11.0, W::Regular), th::MUTED);
            p.galley(Pos2::new(text_right - tg.size().x, meta.top() + 27.0 - tg.size().y / 2.0), tg.clone(), th::MUTED);
            let name_w = text_right - meta.left() - 4.0;
            let name = if r.name.is_empty() { rv::format_id(&r.id) } else { r.name.clone() };
            let mut job = egui::text::LayoutJob::simple_singleline(name, th::font(13.5, W::Semibold), th::TEXT);
            job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
            let ng = ui.painter().layout_job(job);
            ui.painter().galley(meta.min, ng, th::TEXT);
            ui.painter().text(Pos2::new(meta.left(), meta.top() + 19.0), Align2::LEFT_TOP, rv::format_id(&r.id), th::mono(12.0, W::Regular), th::MUTED);
            // border last (hover = accent)
            ui.painter().rect_stroke(rect, th::RADIUS, Stroke::new(1.0, if hover { th::ACCENT } else { th::BORDER }), StrokeKind::Inside);
            // .tile-remove
            let mut removed = false;
            let xr = Rect::from_min_size(Pos2::new(rect.right() - 6.0 - 24.0, rect.top() + 6.0), Vec2::splat(24.0));
            if hover || ui.rect_contains_pointer(xr) {
                let xresp = ui.interact(xr, Id::unique(("tile-x", &r.id)), Sense::click()).on_hover_text(t("tt.forget"));
                ui.painter().circle_filled(xr.center(), 12.0, Color32::from_black_alpha(140));
                self.icons.paint(ui, "x", Rect::from_center_size(xr.center(), Vec2::splat(12.0)), Color32::WHITE);
                if xresp.clicked() {
                    forget = Some(r.id.clone());
                    removed = true;
                }
            }
            // Wake-on-LAN for a computer that is off (its network cards came from the LAN)
            if state == "offline" && !r.macs.is_empty() && !removed {
                let g = ui.painter().layout_no_wrap(t("lan.wake"), th::font(11.5, W::Semibold), Color32::WHITE);
                let wr = Rect::from_min_size(Pos2::new(thumb.right() - 8.0 - (26.0 + g.size().x + 10.0), thumb.bottom() - 8.0 - 24.0), Vec2::new(26.0 + g.size().x + 10.0, 24.0));
                let wresp = ui.interact(wr, Id::unique(("tile-wake", &r.id)), Sense::click()).on_hover_text(t("tt.wake"));
                ui.painter().rect_filled(wr, 12.0, if wresp.hovered() { th::ACCENT_HOVER } else { th::ACCENT });
                self.icons.paint(ui, "power", Rect::from_center_size(Pos2::new(wr.left() + 15.0, wr.center().y), Vec2::splat(13.0)), Color32::WHITE);
                ui.painter().galley(Pos2::new(wr.left() + 26.0, wr.center().y - g.size().y / 2.0), g, Color32::WHITE);
                if wresp.clicked() {
                    wake = Some(r.id.clone());
                    removed = true;
                }
            }
            if hover {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let in_more = ui.rect_contains_pointer(mr);
            if resp.clicked() && !removed && !in_more {
                connect = Some(r.id.clone());
            }
            // the ⋯ in the corner (AnyDesk's tile menu), or a right click / long press: the
            // other kinds of session
            let mresp = ui.interact(mr, Id::unique(("tile-more", &r.id)), Sense::click()).on_hover_text(t("tt.more"));
            let menu_open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&mresp));
            if mresp.hovered() || menu_open {
                ui.painter().rect_filled(mr, 6.0, th::SURFACE_2);
            }
            self.icons.paint(ui, "more", Rect::from_center_size(mr.center(), Vec2::splat(16.0)), if mresp.hovered() || menu_open { th::TEXT } else { th::MUTED });
            let wake_ok = state == "offline" && !r.macs.is_empty();
            let mut tile_menu = |ui: &mut Ui| {
                if let Some(m) = self.session_menu(ui, wake_ok) {
                    menu_pick = Some((r.id.clone(), m));
                }
                ui.separator();
                if ui.button(t("tt.forget")).clicked() {
                    forget = Some(r.id.clone());
                }
            };
            egui::Popup::menu(&mresp).show(&mut tile_menu);
            resp.context_menu(&mut tile_menu);
        }
        match menu_pick {
            Some((id, Some(m))) => self.core.connect_mode(&id, m),
            Some((id, None)) => wake = Some(id),
            None => {}
        }
        if let Some(id) = forget {
            self.core.store.forget(&id);
        } else if let Some(id) = wake {
            self.core.wake(&id);
        } else if let Some(id) = connect {
            self.core.connect(&id);
        }
    }

    /// (colour, grayscale) textures of a recent tile's JPEG thumbnail.
    fn thumb(&mut self, ctx: &egui::Context, id: &str, jpeg: &[u8]) -> Option<(TextureHandle, TextureHandle)> {
        if jpeg.is_empty() {
            return None;
        }
        if let Some((len, a, b)) = self.thumbs.get(id) {
            if *len == jpeg.len() {
                return Some((a.clone(), b.clone()));
            }
        }
        let img = image::load_from_memory(jpeg).ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let color = ctx.load_texture(format!("thumb-{id}"), egui::ColorImage::from_rgba_unmultiplied(size, &img), TextureOptions::LINEAR);
        // filter: grayscale(.85) brightness(.9)
        let gray: Vec<u8> = img
            .chunks_exact(4)
            .flat_map(|p| {
                let l = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                let m = |c: u8| ((c as f32 * 0.15 + l * 0.85) * 0.9) as u8;
                [m(p[0]), m(p[1]), m(p[2]), 255]
            })
            .collect();
        let gray = ctx.load_texture(format!("thumb-g-{id}"), egui::ColorImage::from_rgba_unmultiplied(size, &gray), TextureOptions::LINEAR);
        self.thumbs.insert(id.to_string(), (jpeg.len(), color.clone(), gray.clone()));
        Some((color, gray))
    }

    fn page_share(&mut self, ui: &mut Ui) {
        if !compact(ui.ctx()) {
            self.page_title(ui, "nav.share");
        }
        if cfg!(any(target_os = "macos", target_os = "android")) {
            let perms = self.core.perms;
            th::panel(ui, |ui| {
                th::h3(ui, &t("perm.title"));
                for (i, (ok, title, hint, btn, screen)) in
                    [(perms.screen, "perm.screen", "perm.screenHint", "perm.open", true), (perms.input, "perm.ax", "perm.axHint", "perm.grant", false)].into_iter().enumerate()
                {
                    if i > 0 {
                        let y = ui.cursor().top();
                        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, th::BORDER));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        th::dot(ui, if ok { th::OK } else { th::DANGER });
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(text(t(title), 13.5, W::Semibold, th::TEXT));
                            ui.label(text(t(hint), 12.0, W::Regular, th::MUTED));
                        });
                        if !ok {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if th::btn(ui, &t(btn), Btn::Normal).clicked() {
                                    crate::platform::perms::request(screen);
                                }
                            });
                        }
                    });
                    ui.add_space(8.0);
                }
            });
        }
        th::panel(ui, |ui| {
            th::h3(ui, &t("share.screen"));
            if self.core.screens.is_empty() {
                th::hint(ui, &t("share.noScreen"));
            }
            let screens = self.core.screens.clone();
            let width = ui.available_width();
            let gap = 12.0;
            let cols = (((width + gap) / (190.0 + gap)).floor() as usize).max(1);
            let w = (width - gap * (cols as f32 - 1.0)) / cols as f32;
            let img_h = (w - 14.0) * 10.0 / 16.0;
            let h = img_h + 14.0 + 22.0;
            let rows = screens.len().div_ceil(cols);
            if rows > 0 {
                let (area, _) = ui.allocate_exact_size(Vec2::new(width, rows as f32 * (h + gap) - gap), Sense::hover());
                for (i, s) in screens.iter().enumerate() {
                    let rect = Rect::from_min_size(area.min + Vec2::new((i % cols) as f32 * (w + gap), (i / cols) as f32 * (h + gap)), Vec2::new(w, h));
                    let on = self.core.screen == Some(s.id) || (self.core.screen.is_none() && i == 0);
                    let resp = ui.interact(rect, Id::unique(("screen", s.id)), Sense::click());
                    let p = ui.painter();
                    p.rect(rect, 8.0, if on { th::ACCENT_SOFT } else { th::SURFACE_2 }, Stroke::new(if on { 2.0 } else { 1.0 }, if on { th::ACCENT } else { th::BORDER }), StrokeKind::Inside);
                    let img = Rect::from_min_size(rect.min + Vec2::splat(7.0), Vec2::new(w - 14.0, img_h));
                    p.rect_filled(img, 4.0, Color32::BLACK);
                    if let Some(tex) = self.screen_thumb(ui.ctx(), s) {
                        ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                    let label = format!("{}  {}×{}", s.name, s.width, s.height);
                    let col = if on { th::TEXT } else { th::MUTED };
                    let mut job = egui::text::LayoutJob::simple_singleline(label, th::font(12.0, if on { W::Medium } else { W::Regular }), col);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(img.width() - 4.0);
                    let g = ui.painter().layout_job(job);
                    ui.painter().galley(Pos2::new(img.left() + 2.0, img.bottom() + 6.0), g, col);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.core.screen = Some(s.id);
                    }
                }
            }
        });
        th::panel(ui, |ui| {
            th::h3(ui, &t("share.defaults"));
            if perm_toggles(ui, &self.icons, &mut self.core.store.perms) {
                self.core.store.save();
            }
        });
        th::panel(ui, |ui| {
            th::h3(ui, &t("share.addr"));
            if th::checkbox(ui, &mut self.core.store.presence, &t("share.presence")).changed() {
                self.core.store.save();
            }
            let mut lan = self.core.store.lan_visible;
            if th::checkbox(ui, &mut lan, &t("lan.visible")).changed() {
                self.core.set_lan_visible(lan);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if th::btn(ui, &t("share.newId"), Btn::Normal).clicked() {
                    self.core.new_id();
                }
                th::hint(ui, &t("share.newIdHint"));
            });
        });
    }

    /// Settings: unattended access (password, TOTP, profile), access list, connection log.
    fn security_panels(&mut self, ui: &mut Ui) {
        th::panel(ui, |ui| {
            th::h3(ui, &t("un.title"));
            th::hint(ui, &t("un.hint"));
            ui.add_space(10.0);
            let has_pw = !self.core.store.unattended.key.is_empty();
            ui.add_enabled_ui(has_pw, |ui| {
                if th::checkbox(ui, &mut self.core.store.unattended.enabled, &t("un.enable")).changed() {
                    self.core.store.save();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                th::dot(ui, if has_pw { th::OK } else { th::MUTED });
                ui.label(text(t(if has_pw { "un.isSet" } else { "un.noPw" }), 12.5, W::Regular, th::MUTED));
            });
            ui.add_space(10.0);
            ui.columns(2, |c| {
                field(&mut c[0], &t("un.pw"), |ui| input(ui, &mut self.un_pw.0, true));
                field(&mut c[1], &t("un.pw2"), |ui| input(ui, &mut self.un_pw.1, true));
            });
            if th::btn(ui, &t("un.set"), Btn::Primary).clicked() {
                let (a, b) = std::mem::take(&mut self.un_pw);
                if a.chars().count() < 8 {
                    self.core.toast(t("un.short"), true);
                } else if a != b {
                    self.core.toast(t("un.mismatch"), true);
                } else {
                    let salt = crate::auth::new_salt();
                    let un = &mut self.core.store.unattended;
                    un.key = hex::encode(crate::auth::derive(&a, &salt));
                    un.salt = salt;
                    un.enabled = true;
                    self.core.store.save();
                    self.core.toast(t("un.saved"), false);
                }
            }
            ui.add_space(16.0);
            ui.label(text(t("un.totp"), 13.5, W::Semibold, th::TEXT));
            ui.add_space(6.0);
            if !self.core.store.unattended.totp.is_empty() {
                ui.horizontal(|ui| {
                    th::dot(ui, th::OK);
                    ui.label(text(t("un.totpOn"), 12.5, W::Regular, th::MUTED));
                    if th::btn(ui, &t("un.totpOff"), Btn::Normal).clicked() {
                        self.core.store.unattended.totp.clear();
                        self.core.store.unattended.trusted.clear();
                        self.core.store.save();
                    }
                });
                // devices that gave the code once ("trust this device")
                ui.add_space(8.0);
                let n = self.core.store.unattended.trusted.len();
                ui.horizontal(|ui| {
                    let label = if n == 0 { t("un.trustedNone") } else { tf("un.trusted", &[("n", &n.to_string())]) };
                    ui.label(text(label, 12.5, W::Regular, th::MUTED));
                    if n > 0 && th::btn(ui, &t("un.trustedClear"), Btn::Normal).clicked() {
                        self.core.store.unattended.trusted.clear();
                        self.core.store.save();
                    }
                });
                if n > 0 {
                    let names: Vec<String> = self.core.store.unattended.trusted.iter().rev().take(5).map(|d| format!("{} · {}", d.name, d.id)).collect();
                    ui.label(text(names.join("\n"), 12.0, W::Regular, th::MUTED));
                }
            } else if let Some(secret) = self.totp_new.clone() {
                th::hint(ui, &t("un.totpHint"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    qr(ui, &crate::auth::otpauth_uri(&secret, &self.core.store.my_id), 168.0);
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(mono(&secret, 12.0, W::Regular, th::MUTED, 0.5)).selectable(true));
                        ui.add_space(8.0);
                        field(ui, &t("auth.code"), |ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.totp_code).char_limit(6).frame(egui::Frame::NONE).font(th::mono(15.0, W::Semibold)).desired_width(120.0));
                        });
                        ui.horizontal(|ui| {
                            if th::btn(ui, &t("un.totpVerify"), Btn::Primary).clicked() {
                                let raw = crate::auth::base32_decode(&secret).unwrap_or_default();
                                let now = (rv::now_ms() / 1000) as u64;
                                let code = self.totp_code.trim().parse::<u32>().ok();
                                if code.is_some_and(|c| [now - 30, now, now + 30].iter().any(|t| crate::auth::totp_at(&raw, *t) == c)) {
                                    self.core.store.unattended.totp = secret.clone();
                                    self.core.store.save();
                                    self.totp_new = None;
                                    self.totp_code.clear();
                                    self.core.toast(t("un.totpDone"), false);
                                } else {
                                    self.core.toast(t("un.totpBad"), true);
                                }
                            }
                            if th::btn(ui, &t("btn.cancel"), Btn::Normal).clicked() {
                                self.totp_new = None;
                            }
                        });
                    });
                });
            } else if th::btn(ui, &t("un.totpAdd"), Btn::Normal).clicked() {
                self.totp_new = Some(crate::auth::new_totp_secret());
                self.totp_code.clear();
            }
            ui.add_space(16.0);
            ui.label(text(t("un.profile"), 13.5, W::Semibold, th::TEXT));
            ui.add_space(6.0);
            if perm_toggles(ui, &self.icons, &mut self.core.store.unattended.perms) {
                self.core.store.save();
            }
        });
        th::panel(ui, |ui| {
            th::h3(ui, &t("acl.title"));
            th::hint(ui, &t("acl.hint"));
            ui.add_space(10.0);
            if th::checkbox(ui, &mut self.core.store.acl_enabled, &t("acl.enable")).changed() {
                self.core.store.save();
            }
            ui.add_space(8.0);
            egui::Frame::new().fill(th::SURFACE_2).stroke(Stroke::new(1.0, th::BORDER_STRONG)).corner_radius(6).inner_margin(Margin::symmetric(10, 9)).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.acl_text).frame(egui::Frame::NONE).font(th::mono(12.0, W::Regular)).hint_text("123 456 789").desired_rows(3).desired_width(f32::INFINITY));
            });
            ui.add_space(10.0);
            if th::btn(ui, &t("btn.save"), Btn::Primary).clicked() {
                let ids: Vec<String> = self.acl_text.lines().map(rv::normalize_id).filter(|i| i.len() == 9).collect();
                self.acl_text = ids.iter().map(|i| rv::format_id(i)).collect::<Vec<_>>().join("\n");
                self.core.store.acl = ids;
                self.core.store.save();
                self.core.toast(t("turn.saved"), false);
            }
        });
        th::panel(ui, |ui| {
            ui.horizontal(|ui| {
                th::h3(ui, &t("log.title"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.core.store.log.is_empty() && th::btn(ui, &t("log.clear"), Btn::Normal).clicked() {
                        self.core.store.log.clear();
                        self.core.store.save();
                    }
                });
            });
            if self.core.store.log.is_empty() {
                th::hint(ui, &t("log.empty"));
            }
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for e in self.core.store.log.iter().take(100) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let incoming = e.dir == "in";
                        pill(ui, th::font(11.0, W::Medium), 0.0, &t(if incoming { "log.in" } else { "log.out" }), if incoming { th::ACCENT } else { th::OK }, if incoming { th::ACCENT_SOFT } else { th::OK_SOFT });
                        let when = chrono::DateTime::from_timestamp_millis(e.ts).map(|d| d.with_timezone(&chrono::Local).format("%d.%m %H:%M").to_string()).unwrap_or_default();
                        ui.label(text(when, 12.0, W::Regular, th::MUTED));
                        ui.label(text(&e.name, 13.0, W::Medium, th::TEXT));
                        ui.label(mono(rv::format_id(&e.id), 12.0, W::Regular, th::MUTED, 0.0));
                        let d = e.secs;
                        let dur = if d >= 3600 { format!("{}h {}m", d / 3600, d % 3600 / 60) } else { format!("{}m {}s", d / 60, d % 60) };
                        ui.label(text(dur, 12.0, W::Regular, th::MUTED));
                        if e.unattended {
                            ui.label(text(t("log.pw"), 12.0, W::Regular, th::WARN));
                        }
                    });
                    ui.add_space(4.0);
                }
            });
        });
    }

    /// Settings → TURN relay (same fields as the web build).
    fn turn_panel(&mut self, ui: &mut Ui) {
        th::panel(ui, |ui| {
            th::h3(ui, &t("turn.title"));
            th::hint(ui, &t("turn.hint"));
            ui.add_space(10.0);
            th::checkbox(ui, &mut self.turn.enabled, &t("turn.enable"));
            ui.add_space(10.0);
            // .seg: Cloudflare | other TURN
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, (val, key)) in [("cloudflare", "turn.cf"), ("manual", "turn.manual")].into_iter().enumerate() {
                    let on = self.turn.provider == val || (val == "cloudflare" && self.turn.provider.is_empty());
                    let g = ui.painter().layout_no_wrap(t(key), th::font(13.0, if on { W::Medium } else { W::Regular }), th::TEXT);
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 28.0, 32.0), Sense::click());
                    let cr = if i == 0 { CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 } } else { CornerRadius { nw: 0, sw: 0, ne: 6, se: 6 } };
                    ui.painter().rect(r, cr, if on { th::ACCENT_SOFT } else { th::SURFACE_2 }, Stroke::new(1.0, if on { th::ACCENT } else { th::BORDER_STRONG }), StrokeKind::Inside);
                    ui.painter().galley(r.center() - g.size() / 2.0, g, if on { th::ACCENT } else { th::TEXT });
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.turn.provider = val.into();
                    }
                }
            });
            ui.add_space(12.0);
            if self.turn.provider == "manual" {
                field(ui, &t("turn.urls"), |ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.turn_urls).frame(egui::Frame::NONE).font(th::mono(12.0, W::Regular)).hint_text("turn:turn.example.com:3478").desired_rows(3).desired_width(f32::INFINITY));
                });
                ui.columns(2, |c| {
                    field(&mut c[0], &t("turn.user"), |ui| input(ui, &mut self.turn.username, false));
                    field(&mut c[1], &t("turn.pass"), |ui| input(ui, &mut self.turn.credential, true));
                });
            } else {
                th::hint(ui, &t("turn.cfSteps").replace("<b>", "").replace("</b>", ""));
                ui.add_space(8.0);
                ui.columns(2, |c| {
                    field(&mut c[0], &t("turn.keyId"), |ui| input(ui, &mut self.turn.cf_key_id, false));
                    field(&mut c[1], &t("turn.token"), |ui| input(ui, &mut self.turn.cf_token, true));
                });
            }
            ui.add_space(12.0);
            let out = self.turn_out.lock().unwrap().clone();
            let testing = out.as_ref().is_some_and(|o| o.1.is_none());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if th::btn(ui, &t("btn.save"), Btn::Primary).clicked() {
                    self.save_turn();
                }
                if th::btn(ui, &t("btn.test"), Btn::Normal).clicked() && !testing {
                    let mut cfg = self.edited_turn();
                    if cfg.cf_key_id != self.core.store.turn.cf_key_id || cfg.cf_token != self.core.store.turn.cf_token {
                        cfg.cf_cache = None;
                    }
                    *self.turn_out.lock().unwrap() = Some(("…".into(), None));
                    let slot = self.turn_out.clone();
                    crate::rt::spawn(async move {
                        let r = crate::turncfg::test(cfg).await;
                        *slot.lock().unwrap() = Some(match r {
                            Ok(s) => (s, Some(true)),
                            Err(e) => (format!("{}\n{e}", t("turn.none")), Some(false)),
                        });
                        bus::wake();
                    });
                }
            });
            if let Some((body, ok)) = out {
                ui.add_space(12.0);
                let border = match ok {
                    Some(true) => th::mix(th::OK, th::BORDER, 0.4),
                    Some(false) => th::mix(th::DANGER, th::BORDER, 0.4),
                    None => th::BORDER,
                };
                egui::Frame::new().fill(th::SURFACE_2).stroke(Stroke::new(1.0, border)).corner_radius(6).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(body).font(th::mono(12.0, W::Regular)).color(th::TEXT)).selectable(true));
                });
            }
        });
    }

    /// The panel's fields as a config (addresses validated like the web build).
    fn edited_turn(&self) -> crate::store::TurnCfg {
        let mut c = self.turn.clone();
        if c.provider.is_empty() {
            c.provider = "cloudflare".into();
        }
        c.urls = self.turn_urls.lines().map(str::trim).filter(|u| (u.starts_with("turn:") || u.starts_with("turns:")) && !u.contains(char::is_whitespace)).map(String::from).collect();
        c.cf_key_id = c.cf_key_id.trim().into();
        c.cf_token = c.cf_token.trim().into();
        c
    }

    fn save_turn(&mut self) {
        let mut c = self.edited_turn();
        let prev = &self.core.store.turn;
        c.cf_cache = if c.cf_key_id == prev.cf_key_id && c.cf_token == prev.cf_token { prev.cf_cache.clone() } else { None };
        let ready = if c.provider == "cloudflare" { !c.cf_key_id.is_empty() && !c.cf_token.is_empty() } else { !c.urls.is_empty() };
        let msg = if !c.enabled {
            (t("turn.off"), false)
        } else if !ready {
            (tf("turn.savedErr", &[("e", &t(if c.provider == "cloudflare" { "turn.needCf" } else { "turn.needUrl" }))]), true)
        } else {
            (t("turn.saved"), false)
        };
        self.turn_urls = c.urls.join("\n");
        self.turn = c.clone();
        self.core.store.turn = c;
        self.core.store.save();
        self.core.toast(msg.0, msg.1);
    }

    fn screen_thumb(&mut self, ctx: &egui::Context, s: &crate::media::capture::Screen) -> Option<TextureHandle> {
        if let Some(t) = self.screen_thumbs.get(&s.id) {
            return Some(t.clone());
        }
        use base64::Engine;
        let b64 = s.thumb.split_once(',')?.1;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let tex = ctx.load_texture(format!("screen-{}", s.id), egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img), TextureOptions::LINEAR);
        self.screen_thumbs.insert(s.id, tex.clone());
        Some(tex)
    }

    /// Settings → startup: run with the system, service (Windows), auto-update.
    fn startup_panel(&mut self, ui: &mut Ui) {
        th::panel(ui, |ui| {
            th::h3(ui, &t("start.title"));
            if self.startup_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
                self.startup_checked = Some(Instant::now());
                let (state, ctx) = (self.startup.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    #[cfg(windows)]
                    let svc = crate::platform::service::installed();
                    #[cfg(not(windows))]
                    let svc = false;
                    *state.lock().unwrap() = (crate::platform::system::autostart_enabled(), svc);
                    ctx.request_repaint();
                });
            }
            let (mut on, _installed) = *self.startup.lock().unwrap();
            if th::checkbox(ui, &mut on, &t("start.autostart")).changed() {
                match crate::platform::system::set_autostart(on) {
                    Ok(()) => self.startup.lock().unwrap().0 = on,
                    Err(e) => self.core.toast(e, true),
                }
            }
            th::hint(ui, &t("start.autostartHint"));
            #[cfg(windows)]
            {
                ui.add_space(10.0);
                let installed = _installed;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if installed {
                        if th::btn(ui, &t("start.svcRemove"), Btn::Normal).clicked() {
                            match crate::platform::service::uninstall() {
                                Ok(()) => {
                                    self.startup.lock().unwrap().1 = false;
                                    self.core.toast(t("start.svcRemoved"), false)
                                }
                                Err(e) => self.core.toast(e, true),
                            }
                        }
                        th::dot(ui, th::OK);
                        ui.label(text(t("start.svcOn"), 13.0, W::Regular, th::MUTED));
                    } else if th::btn(ui, &t("start.svcInstall"), Btn::Primary).clicked() {
                        match crate::platform::service::install() {
                            Ok(()) => {
                                self.startup.lock().unwrap().1 = true;
                                self.core.toast(t("start.svcInstalled"), false)
                            }
                            Err(e) => self.core.toast(e, true),
                        }
                    }
                });
                th::hint(ui, &t("start.svcHint"));
            }
            ui.add_space(12.0);
            let mut au = self.core.store.autoupdate;
            if th::checkbox(ui, &mut au, &t("start.autoupdate")).changed() {
                self.core.store.autoupdate = au;
                self.core.store.save();
            }
            ui.add_space(8.0);
            let state = self.update.lock().unwrap().clone();
            let checking = matches!(state, Some((_, None)));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if th::btn(ui, &t("start.checkNow"), Btn::Normal).clicked() && !checking {
                    self.check_update(true);
                }
                if checking {
                    ui.add(egui::Spinner::new().size(16.0).color(th::MUTED));
                }
            });
            if let Some((msg, ok)) = state.filter(|s| s.1.is_some()) {
                ui.add_space(6.0);
                let (col, open) = match ok {
                    Some(true) => (th::OK, true),
                    _ => (th::MUTED, false),
                };
                ui.label(text(&msg, 12.5, W::Regular, col));
                if open && th::btn(ui, &t("start.install"), Btn::Primary).clicked() {
                    self.do_update();
                }
            }
        });
    }

    /// Checks for a newer ManyDesk now; `manual` shows "up to date" too.
    fn check_update(&mut self, manual: bool) {
        crate::rt::spawn(run_check(self.update.clone(), manual));
    }

    /// Every frame: tell the update scheduler whether a session is open and when the window
    /// was last used, and start the scheduler once. The scheduler runs off the UI thread:
    /// macOS stops drawing a window nobody looks at, and an app left in the background is
    /// exactly the one that should update itself.
    fn update_tick(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering::SeqCst;
        if cfg!(mobile) {
            return;
        }
        // someone at the keyboard or the mouse (not focus changes, repaints or a pointer
        // that only left the window)
        let used = ctx.input(|i| {
            i.pointer.delta() != egui::Vec2::ZERO
                || i.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } | egui::Event::Touch { .. } | egui::Event::Paste(_)))
        });
        if used {
            UPD_INPUT.store(since_start().as_secs(), SeqCst);
        }
        UPD_BUSY.store(self.busy(), SeqCst);
        UPD_AUTO.store(self.core.store.autoupdate, SeqCst);
        if !UPD_STARTED.swap(true, SeqCst) {
            crate::rt::spawn(update_scheduler(self.update.clone()));
        }
    }

    /// Any session open, either way, or a request waiting for an answer.
    fn busy(&self) -> bool {
        !self.core.tabs.is_empty() || !self.core.hosts.is_empty() || self.core.pending.is_some() || !self.core.queue.is_empty()
    }

    /// "Download and install": at once, or when the open sessions have ended.
    fn do_update(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        let Some(u) = UPDATE.lock().unwrap().clone() else { return };
        if self.busy() {
            UPD_NOW.store(true, SeqCst);
            self.core.toast(t("start.updateLater"), false);
            return;
        }
        crate::rt::spawn(install_update(u, self.update.clone()));
    }

    fn page_settings(&mut self, ui: &mut Ui) {
        // a phone: the app bar already says where this is
        if !compact(ui.ctx()) {
            self.page_title(ui, "nav.settings");
        }
        th::panel(ui, |ui| {
            th::h3(ui, &t("set.lang"));
            let mut choice = self.core.store.lang.clone();
            let name = |c: &str| if c == "system" || c.is_empty() { t("lang.system") } else { i18n::languages().into_iter().find(|l| l.0 == c).map(|l| l.1.to_string()).unwrap_or_default() };
            egui::ComboBox::from_id_salt("lang").width(220.0).selected_text(text(name(&choice), 13.5, W::Regular, th::TEXT)).show_ui(ui, |ui| {
                ui.selectable_value(&mut choice, "system".into(), t("lang.system"));
                for (code, native) in i18n::languages() {
                    ui.selectable_value(&mut choice, code.into(), native);
                }
            });
            if choice != self.core.store.lang {
                self.core.store.lang = choice.clone();
                self.core.store.save();
                i18n::set(&choice);
            }
        });
        // starting with the system and the self-update are a desktop's (a phone's app
        // store updates the app)
        if !cfg!(mobile) {
            self.startup_panel(ui);
        }
        self.security_panels(ui);
        th::panel(ui, |ui| {
            th::h3(ui, &t("diag.title"));
            th::hint(ui, &t("diag.hint"));
            ui.add_space(12.0);
            let state = self.diag.lock().unwrap().clone();
            let running = state.as_ref().is_some_and(|s| s.1.is_none());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if th::btn(ui, &t("diag.run"), Btn::Primary).clicked() && !running {
                    *self.diag.lock().unwrap() = Some((t("diag.running"), None));
                    let (slot, stun, rv) = (self.diag.clone(), self.core.store.stun.clone(), serde_json::to_value(self.core.rv.status()).unwrap_or_default());
                    crate::rt::spawn(async move {
                        let (text, ok) = crate::diag::run(stun, rv).await;
                        *slot.lock().unwrap() = Some((text, Some(ok)));
                        bus::wake();
                    });
                }
                if th::btn(ui, &t("diag.copy"), Btn::Normal).clicked() {
                    if let Some((text, Some(_))) = &state {
                        ui.ctx().copy_text(text.clone());
                        self.core.toast(t("toast.copied"), false);
                    }
                }
            });
            if let Some((body, ok)) = state {
                ui.add_space(12.0);
                let border = match ok {
                    Some(true) => th::mix(th::OK, th::BORDER, 0.4),
                    Some(false) => th::mix(th::DANGER, th::BORDER, 0.4),
                    None => th::BORDER,
                };
                egui::Frame::new().fill(th::SURFACE_2).stroke(Stroke::new(1.0, border)).corner_radius(6).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(body).font(th::mono(12.0, W::Regular)).color(th::TEXT)).selectable(true));
                    });
                });
            }
        });
        self.turn_panel(ui);
        th::panel(ui, |ui| {
            th::h3(ui, &t("stun.title"));
            th::hint(ui, &t("stun.hint"));
            ui.add_space(8.0);
            egui::Frame::new().fill(th::SURFACE_2).stroke(Stroke::new(1.0, th::BORDER_STRONG)).corner_radius(6).inner_margin(Margin::symmetric(10, 9)).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.stun_text).frame(egui::Frame::NONE).font(th::mono(12.0, W::Regular)).desired_rows(6).desired_width(f32::INFINITY));
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if th::btn(ui, &t("btn.save"), Btn::Primary).clicked() {
                    let list: Vec<String> = self.stun_text.lines().map(str::trim).filter(|l| l.starts_with("stun:") || l.starts_with("stuns:")).map(String::from).collect();
                    if !list.is_empty() {
                        self.core.store.stun = list;
                        self.core.store.save();
                        self.stun_text = self.core.store.stun.join("\n");
                        self.core.toast(t("toast.saved"), false);
                    }
                }
                if th::btn(ui, &t("btn.default"), Btn::Normal).clicked() {
                    self.core.store.stun = crate::store::DEFAULT_STUN.iter().map(|s| s.to_string()).collect();
                    self.core.store.save();
                    self.stun_text = self.core.store.stun.join("\n");
                }
            });
        });
    }

    // ================================================================ session ===

    fn session_view(&mut self, ui: &mut Ui, key: u32) {
        let ctx = ui.ctx().clone();
        let Some(i) = self.core.tabs.iter().position(|t| t.key == key) else { return };
        let fs = self.fullscreen || compact(&ctx);
        if !fs {
            egui::Panel::top("toolbar").exact_size(TOOLBAR_H).frame(egui::Frame::new().fill(th::SURFACE).inner_margin(Margin::symmetric(10, 0))).show(ui, |ui| {
                let r = ui.max_rect().expand2(Vec2::new(10.0, 0.0));
                ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, th::BORDER));
                self.toolbar(ui, key);
            });
        }
        if self.core.tabs[i].chat.open {
            let label = self.core.tabs[i].label.clone();
            egui::Panel::right("chat").exact_size(chat_width(&ctx)).resizable(false).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
                let (send, close) = chat_column(ui, &self.icons, &mut self.core.tabs[i].chat, &label);
                if close {
                    self.core.tabs[i].chat.open = false;
                }
                if send {
                    self.core.send_chat(Some(key), None);
                }
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(th::STAGE)).show(ui, |ui| {
            let stage = ui.max_rect();
            let ppp = ctx.pixels_per_point();
            let tab = &self.core.tabs[i];
            let mode = tab.view;
            // The host's display in its own pixels (the frame may be downscaled for traffic).
            let native = tab.screen.and_then(|id| tab.screens.iter().find(|s| s.id == id)).filter(|s| s.width > 0 && s.height > 0).map(|s| Vec2::new(s.width as f32, s.height as f32));
            // 1:1 asks the host for the full resolution; the other modes for the stage size.
            let (vw, vh) = match (mode, native) {
                (ViewMode::Actual, Some(n)) => (n.x as u32, n.y as u32),
                (ViewMode::Actual, None) => (16384, 16384),
                _ => ((stage.width() * ppp) as u32, (stage.height() * ppp) as u32),
            };
            self.core.send_view(key, vw, vh);
            self.upload_frame(&ctx, i, key);
            let tab = &self.core.tabs[i];
            if tab.state == TabState::Live && tab.term_open {
                let t_stage = if compact(&ctx) { Rect::from_min_max(stage.min, Pos2::new(stage.right(), stage.bottom() - 84.0)) } else { stage };
                self.terminal(ui, t_stage, key);
                self.fs_toolbar(&ctx, stage, key);
                self.session_fab(ui, stage, key);
                return;
            }
            let tab = &self.core.tabs[i];
            if tab.state == TabState::Live && tab.fm.open {
                // a phone: the menu button keeps a strip of its own under the file manager
                let fm_stage = if compact(&ctx) { Rect::from_min_max(stage.min, Pos2::new(stage.right(), stage.bottom() - 84.0)) } else { stage };
                self.file_manager(ui, fm_stage, key);
                self.fs_toolbar(&ctx, stage, key);
                self.session_fab(ui, stage, key);
                return;
            }
            let tab = &self.core.tabs[i];
            if tab.state != TabState::Live || tab.texture.is_none() {
                self.connecting_card(ui, stage, i, key);
                // the camera does not need the screen (a host that can't capture it still has a face)
                if self.core.tabs.get(i).is_some_and(|t| t.state == TabState::Live) {
                    let peer = self.core.tabs[i].label.clone();
                    self.camera_pip(&ctx, key, stage, &peer);
                }
                self.session_fab(ui, stage, key);
                return;
            }
            let tex = tab.texture.clone().unwrap();
            let control = tab.remote.control;
            let capturing = self.core.capture_tab == Some(key);
            let resp = ui.allocate_rect(stage, Sense::click_and_drag());
            let peer = self.core.tabs[i].label.clone();
            self.camera_pip(&ctx, key, stage, &peer);
            let rect = match mode {
                ViewMode::Fit => fit_rect(stage, tex.size_vec2(), ppp),
                ViewMode::Stretch => stage,
                ViewMode::Actual => {
                    let size = native.unwrap_or_else(|| tex.size_vec2()) / ppp;
                    let over = (size - stage.size()).max(Vec2::ZERO);
                    let mut pan = self.core.tabs[i].pan;
                    if over != Vec2::ZERO {
                        if capturing {
                            // The pointer near an edge scrolls that way (wheel and drag go to the host).
                            if let Some(p) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| stage.contains(*p)) {
                                let push = |d: f32| if d < PAN_EDGE { (PAN_EDGE - d) / PAN_EDGE } else { 0.0 };
                                let dt = ctx.input(|i| i.stable_dt).min(0.05);
                                let dir = Vec2::new(push(p.x - stage.left()) * -1.0 + push(stage.right() - p.x), push(p.y - stage.top()) * -1.0 + push(stage.bottom() - p.y));
                                if dir != Vec2::ZERO {
                                    pan += dir * PAN_SPEED * dt;
                                    ctx.request_repaint();
                                }
                            }
                        } else if resp.hovered() {
                            pan -= resp.drag_delta() + ctx.input(|i| i.smooth_scroll_delta);
                        }
                    }
                    pan = pan.clamp(Vec2::ZERO, over);
                    self.core.tabs[i].pan = pan;
                    // Centered on an axis where the screen is smaller than the stage.
                    let min = Pos2::new(
                        if over.x > 0.0 { stage.left() - pan.x } else { stage.center().x - size.x / 2.0 },
                        if over.y > 0.0 { stage.top() - pan.y } else { stage.center().y - size.y / 2.0 },
                    );
                    Rect::from_min_size(min.round(), size)
                }
            };
            // phones: the pinch zoom over "fit"
            let rect = if cfg!(mobile) && mode == ViewMode::Fit { self.zoomed(key, stage, rect) } else { rect };
            let painter = ui.painter().with_clip_rect(stage);
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            // touchpad: the pointer the finger moves
            if cfg!(mobile) && self.touchpad_for(key) && control {
                let c = self.tp_cursor.get(&key).copied().unwrap_or(Pos2::new(0.5, 0.5));
                paint_arrow(&painter, rect.min + Vec2::new(c.x * rect.width(), c.y * rect.height()));
            }
            if std::env::var_os("MANYDESK_PERF").is_some() {
                let (ts, rp) = (tex.size_vec2(), rect.size() * ppp);
                let line = format!("[draw] texture {}x{} drawn as {:.2}x{:.2} px at ({:.2},{:.2}) px", ts.x, ts.y, rp.x, rp.y, rect.min.x * ppp, rect.min.y * ppp);
                if ctx.data_mut(|d| d.get_temp::<String>(Id::new("perf-draw")) != Some(line.clone())) {
                    eprintln!("{line}");
                    ctx.data_mut(|d| d.insert_temp(Id::new("perf-draw"), line));
                }
            }
            if mode == ViewMode::Actual {
                // thin scroll indicators
                let bar = Color32::from_white_alpha(110);
                if rect.width() > stage.width() + 0.5 {
                    let w = stage.width() * stage.width() / rect.width();
                    let x = stage.left() + (stage.left() - rect.left()) / (rect.width() - stage.width()) * (stage.width() - w);
                    painter.rect_filled(Rect::from_min_size(Pos2::new(x, stage.bottom() - 6.0), Vec2::new(w, 4.0)), 2.0, bar);
                }
                if rect.height() > stage.height() + 0.5 {
                    let h = stage.height() * stage.height() / rect.height();
                    let y = stage.top() + (stage.top() - rect.top()) / (rect.height() - stage.height()) * (stage.height() - h);
                    painter.rect_filled(Rect::from_min_size(Pos2::new(stage.right() - 6.0, y), Vec2::new(4.0, h)), 2.0, bar);
                }
            }
            // Ctrl+Alt+Shift+←/→ switches the host's display (AnyDesk's shortcut); not forwarded.
            let step = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key: k, pressed: true, repeat: false, modifiers, .. } if is_switch_keys(*k, *modifiers) => Some(if *k == egui::Key::ArrowRight { 1 } else { -1 }),
                    _ => None,
                })
            });
            if let Some(step) = step {
                self.core.switch_screen(key, None, step);
            }
            // files dropped on the stage
            let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
            if !dropped.is_empty() {
                self.core.send_files(self.core.tabs[i].sid, dropped);
            }
            if hovering {
                let r = Rect::from_center_size(stage.center(), Vec2::new(260.0, 64.0));
                ui.painter().rect_filled(r, 8.0, Color32::from_white_alpha(242));
                th::dashed_rect(ui, r, th::ACCENT);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, t("sess.drop"), th::font(15.0, W::Regular), th::TEXT);
            }
            if !capturing && control && !cfg!(mobile) && !self.boards.get(&key).is_some_and(|b| b.on) {
                let g = ui.painter().layout_no_wrap(t("sess.click"), th::font(12.5, W::Regular), th::TEXT);
                let hint = Rect::from_center_size(Pos2::new(stage.center().x, stage.bottom() - 20.0 - 16.0), Vec2::new(g.size().x + 28.0, 32.0));
                ui.painter().add(th::shadow().as_shape(hint, CornerRadius::same(16)));
                ui.painter().rect_filled(hint, 16.0, Color32::from_white_alpha(242));
                ui.painter().galley(hint.center() - g.size() / 2.0, g, th::TEXT);
            }
            if self.board(ui, &resp, key, rect, stage) {
                self.fs_toolbar(&ctx, stage, key);
                self.session_fab(ui, stage, key);
                return;
            }
            if resp.clicked() && !capturing && control {
                self.core.set_capture(Some(key));
                return;
            }
            // a phone has no pointer to give back: fingers control the host right away
            if cfg!(mobile) && !capturing && control {
                self.core.set_capture(Some(key));
            }
            if capturing {
                // The local pointer: a small dot that moves instantly (the remote arrow follows).
                ctx.set_cursor_icon(egui::CursorIcon::None);
                if let Some(p) = ctx.input(|i| i.pointer.hover_pos()).filter(|p| stage.contains(*p)) {
                    ui.painter().circle(p, 3.5, Color32::WHITE, Stroke::new(1.5, Color32::from_black_alpha(153)));
                }
                self.forward_input(&ctx, key, rect, rect.intersect(stage));
            }
            self.fs_toolbar(&ctx, stage, key);
            self.session_fab(ui, stage, key);
        });
    }

    /// Full screen: the toolbar slides in from the top edge.
    fn fs_toolbar(&mut self, ctx: &egui::Context, stage: Rect, key: u32) {
        if !self.fullscreen {
            return;
        }
        let at_top = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y <= stage.top() + 3.0);
        if at_top {
            self.tb_hide_at = Some(Instant::now() + Duration::from_millis(700));
        }
        if self.tb_hide_at.is_some_and(|t| Instant::now() < t) {
            self.floating_toolbar(ctx, stage, key);
        }
    }

    fn floating_toolbar(&mut self, ctx: &egui::Context, stage: Rect, key: u32) {
        let w = (stage.width() - 24.0).min(1100.0);
        let r = egui::Area::new(Id::unique("fs-toolbar")).fixed_pos(Pos2::new(stage.center().x - w / 2.0, stage.top())).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new()
                .fill(th::SURFACE)
                .stroke(Stroke::new(1.0, th::BORDER))
                .corner_radius(CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 })
                .shadow(th::shadow_lg())
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(w - 20.0);
                    ui.set_height(TOOLBAR_H - 12.0);
                    self.toolbar(ui, key);
                });
        });
        if r.response.contains_pointer() {
            self.tb_hide_at = Some(Instant::now() + Duration::from_millis(700));
            if ctx.input(|i| i.pointer.any_pressed()) {
                self.core.set_capture(None);
            }
        }
    }

    fn upload_frame(&mut self, ctx: &egui::Context, i: usize, key: u32) {
        let tab = &mut self.core.tabs[i];
        let Some(feed) = &tab.feed else { return };
        let Some(pic) = feed.shared.latest.lock().unwrap().take() else { return };
        let t0 = Instant::now();
        // Opaque pixels: premultiplied and unmultiplied are the same, minus the per-pixel math.
        let img = egui::ColorImage::from_rgba_premultiplied([pic.width, pic.height], &pic.rgba);
        let t1 = Instant::now();
        match &mut tab.texture {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => tab.texture = Some(ctx.load_texture(format!("tab-{key}"), img, TextureOptions::LINEAR)),
        }
        crate::perf::add("colorimage", t1 - t0);
        crate::perf::add("texset", t1.elapsed());
        let n = self.frames.entry(key).or_default();
        *n += 1;
        if *n == 45 {
            let id = tab.peer_id.clone();
            if let Some(j) = thumbnail(&pic) {
                self.core.store.set_thumb(&id, j);
            }
        }
        self.last_pic.insert(key, pic);
    }

    /// .connecting + .conn-card
    fn connecting_card(&mut self, ui: &mut Ui, stage: Rect, i: usize, key: u32) {
        ui.painter().rect_filled(stage, 0.0, th::BG);
        if self.core.tabs[i].auth.is_some() && self.core.tabs[i].state == TabState::Live {
            return self.auth_card(ui, stage, i, key);
        }
        let tab = &self.core.tabs[i];
        let (msg, err) = match &tab.state {
            // the remote computer restarts: wait for it instead of showing an error
            TabState::Failed(_) if tab.reconnect.is_some() => (t("act.waitRestart"), false),
            // connected, but the host can't read its screen
            TabState::Live if tab.no_picture.is_some() => (tab.no_picture.clone().unwrap_or_default(), true),
            TabState::Failed(e) => (e.clone(), true),
            TabState::Requesting => (t("req.sending"), false),
            TabState::Waiting => (t("req.waiting"), false),
            _ => (t("req.accepted"), false),
        };
        let (title, sas) = (rv::format_id(&tab.peer_id), tab.sas.clone());
        let card_w = 380.0_f32.min(stage.width() - 32.0);
        let mut close = false;
        egui::Area::new(Id::unique(("conn", key))).order(egui::Order::Middle).fixed_pos(Pos2::new(stage.center().x - card_w / 2.0, stage.center().y - 130.0)).show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(th::SURFACE)
                .stroke(Stroke::new(1.0, th::BORDER))
                .corner_radius(12)
                .shadow(th::shadow())
                .inner_margin(Margin { left: 26, right: 26, top: 30, bottom: 24 })
                .show(ui, |ui| {
                    ui.set_width(card_w - 52.0);
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        th::spinner(ui, err);
                        ui.add_space(16.0);
                        ui.label(mono(title, 22.0, W::Semibold, th::TEXT, 1.5));
                        ui.add_space(6.0);
                        ui.label(text(msg, 13.5, W::Regular, if err { th::DANGER } else { th::MUTED }));
                        if !sas.is_empty() {
                            ui.add_space(10.0);
                            ui.label(text(t("sas"), 12.5, W::Regular, th::MUTED));
                            ui.label(mono(&sas, 18.0, W::Semibold, th::OK, 1.5));
                        }
                        ui.add_space(20.0);
                        if th::btn(ui, &t(if err { "btn.close" } else { "btn.cancel" }), Btn::Normal).clicked() {
                            close = true;
                        }
                    });
                });
        });
        if close {
            self.core.close_tab(key);
        }
    }

    /// The host's password prompt (unattended access), in place of the connecting card.
    fn auth_card(&mut self, ui: &mut Ui, stage: Rect, i: usize, key: u32) {
        let title = rv::format_id(&self.core.tabs[i].peer_id);
        let card_w = 380.0_f32.min(stage.width() - 32.0);
        let (mut submit, mut close) = (false, false);
        let icons = &self.icons;
        let tab = &mut self.core.tabs[i];
        let Some(a) = tab.auth.as_mut() else { return };
        egui::Area::new(Id::unique(("auth", key))).order(egui::Order::Middle).fixed_pos(Pos2::new(stage.center().x - card_w / 2.0, stage.center().y - 170.0)).show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(th::SURFACE)
                .stroke(Stroke::new(1.0, th::BORDER))
                .corner_radius(12)
                .shadow(th::shadow())
                .inner_margin(Margin { left: 26, right: 26, top: 26, bottom: 22 })
                .show(ui, |ui| {
                    ui.set_width(card_w - 52.0);
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
                        ui.painter().rect_filled(r, 10.0, th::ACCENT_SOFT);
                        icons.paint(ui, "key", Rect::from_center_size(r.center(), Vec2::splat(24.0)), th::ACCENT);
                        ui.add_space(12.0);
                        ui.label(mono(title, 20.0, W::Semibold, th::TEXT, 1.5));
                        ui.add_space(4.0);
                        ui.label(text(t("auth.title"), 13.5, W::Medium, th::TEXT));
                    });
                    ui.add_space(14.0);
                    let pid = Id::unique(("auth-pw", key));
                    field(ui, &t("auth.password"), |ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut a.password).id(pid).password(true).frame(egui::Frame::NONE).font(th::font(13.5, W::Regular)).desired_width(f32::INFINITY));
                        if !a.busy && !a.totp && a.password.is_empty() && !r.has_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = true;
                        }
                    });
                    if a.totp && a.trusted.is_none() {
                        field(ui, &t("auth.code"), |ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut a.code).char_limit(6).frame(egui::Frame::NONE).font(th::mono(15.0, W::Semibold)).desired_width(f32::INFINITY));
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                submit = true;
                            }
                        });
                    }
                    th::checkbox(ui, &mut a.remember, &t("auth.remember"));
                    if a.totp && a.trusted.is_none() {
                        th::checkbox(ui, &mut a.trust, &t("auth.trust"));
                    }
                    if !a.error.is_empty() {
                        ui.add_space(8.0);
                        ui.label(text(&a.error, 12.5, W::Regular, th::DANGER));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let bw = (ui.available_width() - 8.0) / 2.0;
                        if th::btn_sized(ui, &t("btn.cancel"), Btn::Normal, Some(bw)).clicked() {
                            close = true;
                        }
                        let label = if a.busy { "…".to_string() } else { t("auth.login") };
                        if th::btn_sized(ui, &label, Btn::Primary, Some(bw)).clicked() && !a.busy {
                            submit = true;
                        }
                    });
                    ui.add_space(10.0);
                    ui.vertical_centered(|ui| th::hint(ui, &t("auth.hint")));
                });
        });
        if submit {
            self.core.send_auth(key);
        }
        if close {
            self.core.close_tab(key);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, key: u32) {
        let Some(tab) = self.core.tabs.iter().find(|t| t.key == key) else { return };
        let (label, sas, stats, os, control, quality, live) =
            (tab.label.clone(), tab.sas.clone(), tab.stats.clone(), crate::core::os_name(&tab.os).to_string(), tab.remote.control, tab.quality, tab.state == TabState::Live);
        let (mic_on, remote_mic, unread, chat_open) = (tab.mic.is_some(), tab.remote_mic, tab.chat.unread, tab.chat.open);
        let cam_on = tab.cams.mine.is_some();
        let (screens, cur_screen, view) = (tab.screens.clone(), tab.screen, tab.view);
        let (fm_open, fm_busy) = (tab.fm.open, tab.fm.transfers.iter().filter(|t| !t.finished).count());
        let (term_ok, term_on) = (tab.remote.terminal, tab.term_open);
        let (mode, desk) = (tab.mode, tab.mode == crate::core::SessMode::Desktop);
        // no picture: the route and the delay, not fps and codec
        let stats = if desk { stats } else { stats.split(" · ").take(2).collect::<Vec<_>>().join(" · ") };
        let (privacy_on, board_on) = (tab.privacy, self.boards.get(&key).is_some_and(|b| b.on));
        // a phone: the bar scrolls sideways, the name is in the tab already
        let narrow = compact(ui.ctx());
        let bar = |ui: &mut Ui| ui.horizontal_centered(|ui| {
            // .sess-name
            ui.spacing_mut().item_spacing.x = 8.0;
            if live {
                th::pulse(ui);
            } else {
                th::dot(ui, th::WARN);
            }
            if !narrow {
                ui.label(text(&label, 13.5, W::Semibold, th::TEXT));
            }
            if !sas.is_empty() {
                pill(ui, th::mono(12.5, W::Semibold), 1.0, &sas, th::OK, th::OK_SOFT).on_hover_text(t("tt.sas"));
            }
            ui.spacing_mut().item_spacing.x = 4.0;
            // .tb-sep
            let (sep, _) = ui.allocate_exact_size(Vec2::new(17.0, 22.0), Sense::hover());
            ui.painter().vline(sep.center().x, sep.y_range(), Stroke::new(1.0, th::BORDER));
            let r = th::tbbtn(ui, &self.icons, "chat", chat_open, false, &t("tt.chat"));
            th::badge(ui, r.rect, unread);
            if r.clicked() {
                if let Some(t) = self.core.tab(key) {
                    t.chat.open = !t.chat.open;
                    t.chat.unread = 0;
                }
            }
            // a file transfer or terminal session: only its own window, chat and the end
            if desk {
                if th::tbbtn(ui, &self.icons, "mic", mic_on, false, &t(if mic_on { "tt.micOn" } else { "tt.mic" })).clicked() && live {
                    self.core.set_mic(Some(key), None, !mic_on);
                }
                if th::tbbtn(ui, &self.icons, "camera", cam_on, false, &t(if cam_on { "tt.camOn" } else { "tt.cam" })).clicked() && live {
                    self.core.set_camera(Some(key), None, !cam_on);
                }
                let r = th::tbbtn(ui, &self.icons, "file", fm_open, false, &t("tt.fm"));
                th::badge(ui, r.rect, fm_busy);
                if r.clicked() && live {
                    self.core.fm_toggle(key);
                }
                if term_ok && th::tbbtn(ui, &self.icons, "terminal", term_on, false, &t("tt.term")).clicked() && live {
                    self.toggle_term(key);
                }
                // a phone has no keyboard until asked for: typing into the remote screen
                if cfg!(mobile) && th::tbbtn(ui, &self.icons, "keyboard", self.soft_keyboard, false, &t("tt.keyboard")).clicked() && live {
                    self.soft_keyboard = !self.soft_keyboard;
                    if self.soft_keyboard {
                        self.core.set_capture(Some(key));
                    }
                }
                // .tbselect
                ui.add_space(4.0);
                self.icons.show(ui, "gauge", 18.0, th::MUTED);
                let mut q = quality;
                egui::ComboBox::from_id_salt(("q", key))
                    .width(96.0)
                    .selected_text(text(t(&format!("q.{}", q.as_str())), 12.5, W::Regular, th::TEXT))
                    .show_ui(ui, |ui| {
                        for m in [Mode::Auto, Mode::Speed, Mode::Balanced, Mode::Quality] {
                            ui.selectable_value(&mut q, m, t(&format!("q.{}", m.as_str())));
                        }
                    })
                    .response
                    .on_hover_text(t("tt.quality"));
                if q != quality {
                    self.core.set_quality(key, q);
                }
                ui.add_space(4.0);
                self.icons.show(ui, "monitor", 18.0, th::MUTED);
                let mut v = view;
                egui::ComboBox::from_id_salt(("v", key))
                    .width(150.0)
                    .selected_text(text(t(&format!("view.{}", v.as_str())), 12.5, W::Regular, th::TEXT))
                    .show_ui(ui, |ui| {
                        for m in [ViewMode::Fit, ViewMode::Actual, ViewMode::Stretch] {
                            ui.selectable_value(&mut v, m, t(&format!("view.{}", m.as_str())));
                        }
                    })
                    .response
                    .on_hover_text(t("tt.view"));
                if v != view {
                    if let Some(tab) = self.core.tab(key) {
                        tab.view = v;
                        tab.pan = Vec2::ZERO;
                    }
                }
                let r = th::tbbtn(ui, &self.icons, "more", privacy_on || board_on, false, &t("tt.actions"));
                egui::Popup::menu(&r).show(|ui| self.actions_menu(ui, key));
                if th::tbbtn(ui, &self.icons, if self.fullscreen { "shrink" } else { "expand" }, false, false, &t("tt.fs")).clicked() {
                    self.fullscreen = !self.fullscreen;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                }
                // The host's displays (only when it has more than one): click or Ctrl+Alt+Shift+←/→.
                if screens.len() > 1 {
                    let (sep, _) = ui.allocate_exact_size(Vec2::new(9.0, 22.0), Sense::hover());
                    ui.painter().vline(sep.center().x, sep.y_range(), Stroke::new(1.0, th::BORDER));
                    for (n, sc) in screens.iter().enumerate() {
                        let tip = format!("{} · {}×{}\n{}", sc.name, sc.width, sc.height, t("tt.screenKeys"));
                        if th::screenbtn(ui, &self.icons, n + 1, cur_screen == Some(sc.id), &tip).clicked() {
                            self.core.switch_screen(key, Some(sc.id), 0);
                        }
                    }
                }
                if remote_mic {
                    self.icons.show(ui, "speaker", 18.0, th::OK).on_hover_text(t("tt.remoteMic"));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if th::tbbtn(ui, &self.icons, "power", false, true, &t("tt.disconnect")).clicked() {
                    self.core.close_tab(key);
                    if self.fullscreen {
                        self.fullscreen = false;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    }
                }
                ui.add_space(8.0);
                // .stats.inline: whatever room the permission tag leaves
                let tag_w = if os.is_empty() {
                    0.0
                } else {
                    let tag = mode_tag(mode, control);
                    ui.painter().layout_no_wrap(format!("{os} · {tag}"), th::font(11.5, W::Medium), th::TEXT).size().x + 16.0 + 14.0
                };
                let max = (ui.available_width() - tag_w - 8.0).max(0.0);
                let mut job = egui::text::LayoutJob::simple_singleline(stats, th::font(12.0, W::Regular), th::MUTED);
                job.wrap = egui::text::TextWrapping::truncate_at_width(max);
                // Laid out by hand: a Label would re-wrap to the whole remaining width.
                let g = ui.painter().layout_job(job);
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, th::MUTED);
                if !os.is_empty() {
                    ui.add_space(6.0);
                    let (tag, fg, bg) = if control || !desk { (mode_tag(mode, control), th::OK, th::OK_SOFT) } else { (t("perm.viewOnly"), th::MUTED, th::SURFACE_2) };
                    pill(ui, th::font(11.5, W::Medium), 0.0, &format!("{os} · {tag}"), fg, bg);
                }
            });
        });
        if narrow {
            egui::ScrollArea::horizontal().id_salt(("toolbar", key)).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| bar(ui));
        } else {
            bar(ui);
        }
    }

    /// Captured pointer and keys → the host (positions normalized to the remote screen).
    /// `rect` is where the whole remote screen is drawn (in 1:1 it reaches past the stage),
    /// `visible` the part of it on screen: only events there are sent.
    fn forward_input(&mut self, ctx: &egui::Context, key: u32, rect: Rect, visible: Rect) {
        let swap = self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| !t.os.is_empty() && (t.os == "darwin") != self.mac_local);
        // Translated keyboard: characters as typed here go over as text; only keys that
        // type nothing (arrows, Enter, shortcuts…) go as physical keys.
        let text_mode = self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| t.kbd_text);
        let typed = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_))));
        let norm = |p: Pos2| ((p.x - rect.left()) / rect.width(), (p.y - rect.top()) / rect.height());
        let events = ctx.input(|i| i.events.clone());
        let mut moved = None;
        let mut out = Vec::new();
        let touch = cfg!(mobile);
        if touch {
            // the whole stage takes fingers in touchpad mode (the pointer is where it is)
            let area = if self.touchpad_for(key) { visible.union(ctx.content_rect()) } else { visible };
            self.touch_gestures(ctx, &events, key, rect, area, &mut out);
        }
        for ev in events {
            match ev {
                // a finger: see touch_gestures
                egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } if touch => {}
                // the camera window floats over the picture
                egui::Event::PointerMoved(p) | egui::Event::PointerButton { pos: p, pressed: true, .. } if self.pip_rects.iter().any(|r| r.contains(p)) => {}
                egui::Event::PointerMoved(p) if visible.contains(p) => moved = Some(norm(p)),
                egui::Event::PointerButton { pos, button, pressed, .. } if visible.contains(pos) => {
                    let b = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Middle => 1,
                        egui::PointerButton::Secondary => 2,
                        _ => continue,
                    };
                    let (x, y) = norm(pos);
                    out.push(json!({ "t": if pressed { "down" } else { "up" }, "b": b, "c": 1, "x": x, "y": y }));
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    let k = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 400.0,
                    };
                    out.push(json!({ "t": "scroll", "dx": -delta.x * k, "dy": -delta.y * k }));
                }
                egui::Event::Key { key: k, modifiers, .. } if is_switch_keys(k, modifiers) => {}
                // the release of a Cmd/Ctrl+C/X/V: its press came as Copy/Cut/Paste (below),
                // or as nothing at all when the clipboard holds files
                egui::Event::Key { key: k, physical_key, pressed: false, modifiers, .. } if matches!(physical_key.unwrap_or(k), egui::Key::C | egui::Key::X | egui::Key::V) && (self.shortcut_down || modifiers.command) => {
                    let code = keys::code(physical_key.unwrap_or(k)).unwrap_or("KeyV");
                    if std::mem::take(&mut self.files_pasted) {
                        continue;
                    }
                    if !std::mem::take(&mut self.shortcut_down) {
                        // copied files: they go over and the host pastes them
                        if code == "KeyV" && self.core.paste_files(key) {
                            continue;
                        }
                        out.push(json!({ "t": "key", "code": code, "d": true }));
                    }
                    out.push(json!({ "t": "key", "code": code, "d": false }));
                }
                egui::Event::Text(s) if text_mode => out.push(json!({ "t": "text", "s": s })),
                egui::Event::Key { key: k, physical_key, modifiers, .. } if text_mode && keys::code(physical_key.unwrap_or(k)).is_some_and(|c| types_text(c, modifiers, typed)) => {}
                egui::Event::Key { key: k, physical_key, pressed, .. } => {
                    if let Some(code) = keys::code(physical_key.unwrap_or(k)) {
                        let code = if swap { keys::swap_mods(code) } else { code };
                        out.push(json!({ "t": "key", "code": code, "d": pressed }));
                    }
                }
                // Cmd/Ctrl held is already over there; the key goes up with its release
                egui::Event::Copy | egui::Event::Cut => {
                    self.shortcut_down = true;
                    out.push(json!({ "t": "key", "code": if matches!(ev, egui::Event::Copy) { "KeyC" } else { "KeyX" }, "d": true }));
                }
                // text copied here a moment ago: it goes first, so the other side pastes it
                // and not what it had (the clipboard watcher would be up to 0.7 s late)
                // (files copied in Finder come as their names: those files go instead)
                egui::Event::Paste(_) | egui::Event::PasteImage(_) if self.core.paste_files(key) => self.files_pasted = true,
                egui::Event::Paste(text) => {
                    self.shortcut_down = true;
                    self.core.paste(key, Some(&text), &mut out);
                }
                egui::Event::PasteImage(_) => {
                    self.shortcut_down = true;
                    self.core.paste(key, None, &mut out);
                }
                egui::Event::WindowFocused(false) => self.core.set_capture(None),
                _ => {}
            }
        }
        if let Some((x, y)) = moved {
            out.insert(0, json!({ "t": "move", "x": x, "y": y }));
        }
        self.core.input_many(key, out);
    }

    /// A finger on the remote screen, played as a mouse. Touchpad mode (AnyDesk's default):
    /// the finger moves the pointer, a tap clicks where it is, a tap and then a press drags.
    /// Touch mode: a tap clicks under the finger, a drag drags. Both: holding still is a right
    /// click, two fingers scroll — or zoom and pan the picture.
    fn touch_gestures(&mut self, ctx: &egui::Context, events: &[egui::Event], key: u32, rect: Rect, area: Rect, out: &mut Vec<serde_json::Value>) {
        const SLOP: f32 = 10.0;
        let norm = |p: Pos2| (((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
        let btn = |t: &str, b: i32, (x, y): (f32, f32)| json!({ "t": t, "b": b, "c": 1, "x": x, "y": y });
        let mv = |(x, y): (f32, f32)| json!({ "t": "move", "x": x, "y": y });
        let touchpad = self.touchpad_for(key);
        // a phone over there: a long press is a long press, not a right click (its "back")
        let phone = self.remote_is_phone(key);
        let fab: Vec<Rect> = self.fab_rects.iter().chain(&self.pip_rects).copied().collect();
        let mut cur = self.tp_cursor.get(&key).copied().unwrap_or(Pos2::new(0.5, 0.5));
        let at_cur = |c: Pos2| (c.x, c.y);
        for ev in events {
            match ev {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } if area.contains(*pos) && !fab.iter().any(|r| r.contains(*pos)) => {
                    let hold = touchpad && self.tp_last_tap.is_some_and(|t| t.elapsed() < Duration::from_millis(350));
                    self.touch = Some(Touch { start: *pos, last: *pos, at: Instant::now(), dragging: false, done: false, hold, down_sent: false });
                }
                egui::Event::PointerMoved(p) => {
                    let Some(t) = self.touch.as_mut() else { continue };
                    let prev = t.last;
                    t.last = *p;
                    if t.done {
                        continue;
                    }
                    if !t.dragging && t.start.distance(*p) > SLOP {
                        t.dragging = true;
                        if !touchpad {
                            if !t.down_sent {
                                out.push(mv(norm(t.start)));
                                out.push(btn("down", 0, norm(t.start)));
                            }
                        } else if t.hold {
                            out.push(mv(at_cur(cur)));
                            out.push(btn("down", 0, at_cur(cur)));
                            t.down_sent = true;
                        }
                    }
                    if t.dragging {
                        if touchpad {
                            let d = *p - prev;
                            cur.x = (cur.x + d.x * TP_SPEED / rect.width()).clamp(0.0, 1.0);
                            cur.y = (cur.y + d.y * TP_SPEED / rect.height()).clamp(0.0, 1.0);
                            out.push(mv(at_cur(cur)));
                        } else {
                            out.push(mv(norm(*p)));
                        }
                    }
                }
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => {
                    let Some(t) = self.touch.take() else { continue };
                    if touchpad {
                        if t.down_sent {
                            out.push(btn("up", 0, at_cur(cur)));
                        } else if !t.dragging && !t.done {
                            out.push(mv(at_cur(cur)));
                            out.push(btn("down", 0, at_cur(cur)));
                            out.push(btn("up", 0, at_cur(cur)));
                            self.tp_last_tap = Some(Instant::now());
                        }
                    } else if t.dragging || t.down_sent {
                        out.push(btn("up", 0, norm(*pos)));
                    } else if !t.done {
                        out.push(mv(norm(t.start)));
                        out.push(btn("down", 0, norm(t.start)));
                        out.push(btn("up", 0, norm(t.start)));
                    }
                }
                _ => {}
            }
        }
        // two fingers: zoom (pinch) and pan the picture once zoomed, else scroll
        if let Some(m) = ctx.multi_touch().filter(|m| m.num_touches >= 2) {
            if let Some(t) = self.touch.as_mut() {
                if (!touchpad && t.dragging) || t.down_sent {
                    out.push(btn("up", 0, if touchpad { at_cur(cur) } else { norm(t.last) }));
                }
                t.dragging = false;
                t.down_sent = false;
                t.done = true;
            }
            let (z, min) = self.zoom.get(&key).copied().unwrap_or((1.0, rect.min));
            if (m.zoom_delta - 1.0).abs() > 0.003 {
                let nz = (z * m.zoom_delta).clamp(1.0, 6.0);
                let k = nz / z;
                self.zoom.insert(key, (nz, m.center_pos - (m.center_pos - rect.min) * k));
            } else if z > 1.01 {
                self.zoom.insert(key, (z, min + m.translation_delta));
            } else if m.translation_delta != Vec2::ZERO {
                let d = m.translation_delta;
                out.push(json!({ "t": "scroll", "dx": -d.x * 2.0, "dy": -d.y * 2.0 }));
            }
            ctx.request_repaint();
        }
        // held still: a right click
        if let Some(t) = self.touch.as_mut().filter(|t| !t.dragging && !t.done) {
            if t.at.elapsed() > Duration::from_millis(550) && phone && !touchpad {
                // held down until the finger lifts (and dragged if it moves)
                t.dragging = true;
                t.down_sent = true;
                out.push(mv(norm(t.start)));
                out.push(btn("down", 0, norm(t.start)));
            } else if t.at.elapsed() > Duration::from_millis(550) {
                t.done = true;
                let p = if touchpad { at_cur(cur) } else { norm(t.start) };
                out.push(mv(p));
                out.push(btn("down", 2, p));
                out.push(btn("up", 2, p));
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        // zoomed in: the picture follows the pointer
        if touchpad {
            if let Some((z, min)) = self.zoom.get(&key).copied().filter(|z| z.0 > 1.01) {
                let p = rect.min + Vec2::new(cur.x * rect.width(), cur.y * rect.height());
                let inner = area.shrink(40.0);
                let shift = Vec2::new(
                    if p.x < inner.left() { inner.left() - p.x } else if p.x > inner.right() { inner.right() - p.x } else { 0.0 },
                    if p.y < inner.top() { inner.top() - p.y } else if p.y > inner.bottom() { inner.bottom() - p.y } else { 0.0 },
                );
                if shift != Vec2::ZERO {
                    self.zoom.insert(key, (z, min + shift));
                    ctx.request_repaint();
                }
            }
        }
        self.tp_cursor.insert(key, cur);
    }

    /// Whether the other side is a phone (Android): it is touched, not pointed at.
    fn remote_is_phone(&self, key: u32) -> bool {
        self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| matches!(t.os.as_str(), "android" | "ios"))
    }

    /// Touchpad mode for session `key`: the setting, but a phone over there is always touched
    /// directly (a pointer means nothing on it).
    fn touchpad_for(&self, key: u32) -> bool {
        self.touchpad && !self.remote_is_phone(key)
    }

    /// The picture under a pinch zoom: `fit` scaled by the session's zoom, kept on the stage.
    fn zoomed(&mut self, key: u32, stage: Rect, fit: Rect) -> Rect {
        let Some((z, min)) = self.zoom.get(&key).copied() else { return fit };
        if z <= 1.001 {
            self.zoom.remove(&key);
            return fit;
        }
        let size = fit.size() * z;
        let axis = |min: f32, len: f32, lo: f32, hi: f32| if len <= hi - lo { (lo + hi - len) / 2.0 } else { min.clamp(hi - len, lo) };
        let min = Pos2::new(axis(min.x, size.x, stage.left(), stage.right()), axis(min.y, size.y, stage.top(), stage.bottom()));
        self.zoom.insert(key, (z, min));
        Rect::from_min_size(min, size)
    }

    /// Phones: the session's round menu button (AnyDesk's), bottom right, and its buttons
    /// above it when open — keyboard, files, chat, the pointer mode, actions, home, end.
    fn session_fab(&mut self, ui: &mut Ui, stage: Rect, key: u32) {
        if !compact(ui.ctx()) {
            return;
        }
        self.fab_rects.clear();
        let c = Pos2::new(stage.right() - 40.0, stage.bottom() - 48.0);
        let main = Rect::from_center_size(c, Vec2::splat(56.0));
        let p = ui.painter().clone();
        let mut items: Vec<(&'static str, String, bool)> = Vec::new();
        if self.fab_open {
            let (fm_open, chat_open, live) = self.core.tabs.iter().find(|t| t.key == key).map(|t| (t.fm.open, t.chat.open, t.state == TabState::Live)).unwrap_or((false, false, false));
            let (term_ok, term_on) = self.core.tabs.iter().find(|t| t.key == key).map(|t| (t.remote.terminal, t.term_open)).unwrap_or((false, false));
            let (mic_on, cam_on) = self.core.tabs.iter().find(|t| t.key == key).map(|t| (t.mic.is_some(), t.cams.mine.is_some())).unwrap_or((false, false));
            items.push(("power", t("tt.disconnect"), false));
            items.push(("home", t("home"), false));
            let desk = self.core.tabs.iter().find(|t| t.key == key).is_none_or(|t| t.mode == crate::core::SessMode::Desktop);
            if live && !desk {
                items.push(("chat", t("tt.chat"), chat_open));
            }
            if live && desk {
                items.push(("more", t("tt.actions"), false));
                items.push(("chat", t("tt.chat"), chat_open));
                items.push(("file", t("tt.fm"), fm_open));
                if term_ok {
                    items.push(("terminal", t("tt.term"), term_on));
                }
                items.push(("mic", t(if mic_on { "tt.micOn" } else { "tt.mic" }), mic_on));
                items.push(("camera", t(if cam_on { "tt.camOn" } else { "tt.cam" }), cam_on));
                if !self.remote_is_phone(key) {
                    items.push(("mouse", t(if self.touchpad { "mob.touchpad" } else { "mob.touch" }), self.touchpad));
                }
                items.push(("keyboard", t("tt.keyboard"), self.soft_keyboard));
            }
            // dim the session behind the menu
            p.rect_filled(stage, 0.0, Color32::from_black_alpha(90));
        }
        let mut clicked: Option<&'static str> = None;
        let mut more_resp: Option<egui::Response> = None;
        // as many above the button as fit (a phone turned sideways has little height), the
        // rest in columns further left, each as wide as the widest label needs
        let per_col = (((c.y - 66.0 - (stage.top() + 28.0)) / 56.0).floor() as usize + 1).max(1);
        let widest = items.iter().map(|(_, l, _)| p.layout_no_wrap(l.clone(), th::font(13.0, W::Medium), th::TEXT).size().x).fold(0.0, f32::max);
        let col_w = widest + 20.0 + 12.0 + 44.0 + 16.0;
        for (n, (icon, label, on)) in items.iter().enumerate() {
            let bc = Pos2::new(c.x - (n / per_col) as f32 * col_w, c.y - 66.0 - (n % per_col) as f32 * 56.0);
            let b = Rect::from_center_size(bc, Vec2::splat(44.0));
            let resp = ui.interact(b, Id::unique(("fab-item", key, n)), Sense::click());
            p.add(th::shadow().as_shape(b, CornerRadius::same(22)));
            p.circle_filled(bc, 22.0, if *on { th::ACCENT } else if *icon == "power" { th::DANGER } else { th::SURFACE });
            self.icons.paint(ui, icon, Rect::from_center_size(bc, Vec2::splat(20.0)), if *on || *icon == "power" { Color32::WHITE } else { th::TEXT });
            let g = p.layout_no_wrap(label.clone(), th::font(13.0, W::Medium), th::TEXT);
            let lr = Rect::from_min_size(Pos2::new(b.left() - 12.0 - g.size().x - 20.0, bc.y - 14.0), Vec2::new(g.size().x + 20.0, 28.0));
            p.rect_filled(lr, 14.0, th::SURFACE);
            p.galley(Pos2::new(lr.left() + 10.0, bc.y - g.size().y / 2.0), g, th::TEXT);
            let lresp = ui.interact(lr, Id::unique(("fab-label", key, n)), Sense::click());
            self.fab_rects.push(b.union(lr));
            if resp.clicked() || lresp.clicked() {
                clicked = Some(icon);
            }
            if *icon == "more" {
                more_resp = Some(resp.union(lresp));
            }
        }
        let resp = ui.interact(main, Id::unique(("fab", key)), Sense::click());
        p.add(th::shadow_lg().as_shape(main, CornerRadius::same(28)));
        p.circle_filled(c, 28.0, th::ACCENT);
        self.icons.paint(ui, if self.fab_open { "x" } else { "monitor" }, Rect::from_center_size(c, Vec2::splat(24.0)), Color32::WHITE);
        self.fab_rects.push(main);
        if self.fab_open {
            self.fab_rects.push(stage); // the dimmed stage closes the menu
        }
        if let Some(r) = &more_resp {
            egui::Popup::menu(r).show(|ui| self.actions_menu(ui, key));
        }
        if resp.clicked() {
            self.fab_open = !self.fab_open;
        } else if self.fab_open && clicked.is_none() && ui.input(|i| i.pointer.any_click()) && more_resp.as_ref().is_none_or(|r| !egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(r))) {
            self.fab_open = false;
        }
        match clicked {
            Some("power") => {
                self.fab_open = false;
                self.core.close_tab(key);
            }
            Some("home") => {
                self.fab_open = false;
                self.core.active = None;
                self.core.set_capture(None);
            }
            Some("keyboard") => {
                self.soft_keyboard = !self.soft_keyboard;
                self.fab_open = false;
            }
            Some("file") => {
                self.core.fm_toggle(key);
                self.fab_open = false;
            }
            Some("terminal") => {
                self.toggle_term(key);
                self.fab_open = false;
            }
            Some("chat") => {
                if let Some(t) = self.core.tab(key) {
                    t.chat.open = !t.chat.open;
                    t.chat.unread = 0;
                }
                self.fab_open = false;
            }
            Some("mic") => {
                let on = self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| t.mic.is_some());
                self.core.set_mic(Some(key), None, !on);
                self.fab_open = false;
            }
            Some("camera") => {
                let on = self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| t.cams.mine.is_some());
                self.core.set_camera(Some(key), None, !on);
                self.fab_open = false;
            }
            Some("mouse") => {
                self.touchpad = !self.touchpad;
                self.core.store.touchpad = self.touchpad;
                self.core.store.save();
            }
            _ => {}
        }
    }

    // ================================================================== cameras ===

    /// Host: each viewer's camera, floating over the window.
    fn host_cameras(&mut self, ctx: &egui::Context) {
        // below the top bar (the address field stays reachable)
        let mut area = ctx.content_rect().shrink(8.0);
        area.min.y += 56.0;
        let on: Vec<(u32, String)> = self.core.hosts.iter().filter(|h| h.cams.remote).map(|h| (h.key, h.req.name.clone())).collect();
        for (key, name) in on {
            self.camera_pip(ctx, key, area, &name);
        }
    }

    /// The other side's camera in a small window over `area` (AnyDesk's video next to the
    /// remote screen): dragged anywhere, folded to a round button and back, resized from
    /// its corner.
    fn camera_pip(&mut self, ctx: &egui::Context, key: u32, area: Rect, name: &str) {
        let cams = if let Some(t) = self.core.tabs.iter_mut().find(|t| t.key == key) {
            &mut t.cams
        } else if let Some(h) = self.core.hosts.iter_mut().find(|h| h.key == key) {
            &mut h.cams
        } else {
            return;
        };
        if !cams.remote {
            return;
        }
        let pic = cams.feed.lock().unwrap().as_ref().and_then(|f| f.shared.latest.lock().unwrap().take());
        if let Some(pic) = pic {
            let img = egui::ColorImage::from_rgba_premultiplied([pic.width, pic.height], &pic.rgba);
            match &mut cams.texture {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => cams.texture = Some(ctx.load_texture(format!("cam-{key}"), img, TextureOptions::LINEAR)),
            }
        }
        let tex = cams.texture.clone();
        let small = compact(ctx);
        let max_w = (area.width() * 0.6).clamp(120.0, 640.0);
        let mut pip = *self.pips.entry(key).or_insert(Pip { width: if small { 132.0 } else { 260.0 }, folded: false });
        let aspect = tex.as_ref().map(|t| t.size_vec2()).filter(|s| s.x > 0.0).map(|s| s.y / s.x).unwrap_or(0.75);
        pip.width = pip.width.clamp(112.0, max_w);
        let size = if pip.folded { Vec2::splat(48.0) } else { Vec2::new(pip.width, (pip.width * aspect).round()) };
        // top right, under the corner of the stage (the phone's menu button is at the bottom)
        let default = Pos2::new(area.right() - 16.0, area.top() + 16.0);
        let icons = &self.icons;
        let shown = egui::Area::new(Id::unique(("cam-pip", key)))
            .order(egui::Order::Foreground)
            .movable(true)
            .constrain_to(area)
            .pivot(Align2::RIGHT_TOP)
            .default_pos(default)
            .show(ctx, |ui| {
                let (rect, body) = ui.allocate_exact_size(size, Sense::click());
                let p = ui.painter();
                if pip.folded {
                    p.add(th::shadow_lg().as_shape(rect, CornerRadius::same(24)));
                    p.circle_filled(rect.center(), 24.0, th::ACCENT);
                    icons.paint(ui, "camera", Rect::from_center_size(rect.center(), Vec2::splat(22.0)), Color32::WHITE);
                    if body.on_hover_text(tf("cam.show", &[("who", name)])).clicked() {
                        pip.folded = false;
                    }
                    return;
                }
                p.add(th::shadow_lg().as_shape(rect, CornerRadius::same(10)));
                match &tex {
                    Some(t) => {
                        egui::Image::new((t.id(), t.size_vec2())).corner_radius(10).paint_at(ui, rect);
                    }
                    None => {
                        p.rect_filled(rect, 10.0, Color32::from_gray(28));
                        icons.paint(ui, "camera", Rect::from_center_size(rect.center(), Vec2::splat(28.0)), Color32::from_gray(140));
                    }
                }
                p.rect_stroke(rect, 10.0, Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);
                // who it is, bottom left
                let g = p.layout_no_wrap(name.to_string(), th::font(11.5, W::Medium), Color32::WHITE);
                let lw = (g.size().x + 14.0).min(rect.width() - 16.0);
                let lr = Rect::from_min_size(Pos2::new(rect.left() + 8.0, rect.bottom() - 8.0 - 22.0), Vec2::new(lw, 22.0));
                p.rect_filled(lr, 11.0, Color32::from_black_alpha(140));
                p.with_clip_rect(lr.shrink(4.0)).galley(Pos2::new(lr.left() + 7.0, lr.center().y - g.size().y / 2.0), g, Color32::WHITE);
                let hovered = ui.rect_contains_pointer(rect) || small;
                if hovered {
                    // fold, top right
                    let fr = Rect::from_center_size(Pos2::new(rect.right() - 18.0, rect.top() + 18.0), Vec2::splat(26.0));
                    let fresp = ui.interact(fr, Id::unique(("cam-fold", key)), Sense::click());
                    p.circle_filled(fr.center(), 13.0, Color32::from_black_alpha(if fresp.hovered() { 200 } else { 140 }));
                    icons.paint(ui, "shrink", fr.shrink(6.0), Color32::WHITE);
                    if fresp.on_hover_text(t("cam.hide")).clicked() {
                        pip.folded = true;
                    }
                    // resize, bottom right
                    let gr = Rect::from_min_size(rect.right_bottom() - Vec2::splat(20.0), Vec2::splat(20.0));
                    let gresp = ui.interact(gr, Id::unique(("cam-grip", key)), Sense::drag());
                    let c = if gresp.hovered() || gresp.dragged() { Color32::WHITE } else { Color32::from_white_alpha(170) };
                    for d in [5.0, 10.0] {
                        p.line_segment([gr.right_bottom() - Vec2::new(d + 4.0, 4.0), gr.right_bottom() - Vec2::new(4.0, d + 4.0)], Stroke::new(1.5, c));
                    }
                    if gresp.hovered() || gresp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                    if gresp.dragged() {
                        let d = gresp.drag_delta();
                        pip.width = (pip.width + (d.x + d.y / aspect) / 2.0).clamp(112.0, max_w);
                    }
                }
                // a double click: small ↔ big
                if body.double_clicked() {
                    pip.width = if pip.width < max_w * 0.75 { max_w } else if small { 132.0 } else { 260.0 };
                }
            });
        self.pips.insert(key, pip);
        self.pip_rects.push(shown.response.rect);
    }

    // ============================================================= file manager ===

    /// AnyDesk-style file transfer: this computer on the left, the host on the right, the
    /// copies in progress underneath. Takes the place of the remote screen while open.
    fn file_manager(&mut self, ui: &mut Ui, stage: Rect, key: u32) {
        if compact(ui.ctx()) {
            return self.file_manager_mobile(ui, stage, key);
        }
        let ctx = ui.ctx().clone();
        let Some(i) = self.core.tabs.iter().position(|t| t.key == key) else { return };
        ui.painter().rect_filled(stage, 0.0, th::BG);
        let (allowed, label, ntr) = {
            let t = &self.core.tabs[i];
            (t.remote.files, t.label.clone(), t.fm.transfers.len())
        };
        let inner = stage.shrink(14.0);
        let tr_h = if ntr == 0 { 0.0 } else { 58.0 + FM_TR_ROW * ntr.min(3) as f32 };
        let panes = Rect::from_min_max(inner.min, Pos2::new(inner.right(), inner.bottom() - if ntr > 0 { tr_h + 12.0 } else { 0.0 }));
        let half = ((panes.width() - 12.0) / 2.0).floor();
        let left = Rect::from_min_size(panes.min, Vec2::new(half, panes.height()));
        let right = Rect::from_min_max(Pos2::new(left.right() + 12.0, panes.top()), panes.max);
        let mut acts: Vec<(bool, FmAct)> = Vec::new();
        self.fm_pane(ui, left, i, false, &t("fm.local"), allowed, &mut acts);
        if allowed {
            self.fm_pane(ui, right, i, true, &label, true, &mut acts);
        } else {
            ui.painter().rect(right, th::RADIUS, th::SURFACE, Stroke::new(1.0, th::BORDER), StrokeKind::Inside);
            let r = Rect::from_center_size(right.center(), Vec2::new((right.width() - 40.0).max(80.0), 80.0));
            ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
                ui.vertical_centered(|ui| {
                    self.icons.show(ui, "shield", 28.0, th::MUTED);
                    ui.add_space(8.0);
                    ui.label(text(t("fm.noPerm"), 13.0, W::Regular, th::MUTED));
                });
            });
        }
        if ntr > 0 {
            let r = Rect::from_min_max(Pos2::new(inner.left(), inner.bottom() - tr_h), inner.max);
            self.fm_transfers(ui, r, i, &mut acts);
        }
        // files dropped from the desktop go to the host's open folder
        let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
        if allowed && hovering {
            ui.painter().rect_filled(right, th::RADIUS, th::ACCENT_SOFT.gamma_multiply(0.9));
            th::dashed_rect(ui, right.shrink(6.0), th::ACCENT);
            ui.painter().text(right.center(), Align2::CENTER_CENTER, t("fm.drop"), th::font(14.0, W::Medium), th::ACCENT);
        }
        if allowed && !dropped.is_empty() {
            self.core.fm_copy(key, true, Some(dropped.iter().map(|p| p.to_string_lossy().to_string()).collect()));
        }
        for (remote, a) in acts {
            self.fm_apply(&ctx, key, remote, a);
        }
        if self.core.tabs.get(i).is_some_and(|t| t.fm.busy()) {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        self.fm_dialog(&ctx);
    }

    /// The file manager on a phone, the way AnyDesk's app does it: one side at a time (this
    /// device / the remote) under a switch, big rows — a tap opens a folder, a long press
    /// starts selecting — the actions along the bottom and the copies above them.
    fn file_manager_mobile(&mut self, ui: &mut Ui, stage: Rect, key: u32) {
        let ctx = ui.ctx().clone();
        let Some(i) = self.core.tabs.iter().position(|t| t.key == key) else { return };
        ui.painter().rect_filled(stage, 0.0, th::BG);
        let side_id = Id::unique(("fm-side", key));
        let mut remote = ctx.data(|d| d.get_temp::<bool>(side_id)).unwrap_or(true);
        let (allowed, label) = (self.core.tabs[i].remote.files, self.core.tabs[i].label.clone());
        let mut acts: Vec<(bool, FmAct)> = Vec::new();
        let mut child = ui.new_child(UiBuilder::new().max_rect(stage.shrink2(Vec2::new(12.0, 10.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing.y = 8.0;
        let w = ui.available_width();

        // this device | the remote
        let (sw, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::hover());
        ui.painter().rect_filled(sw, 10.0, th::SURFACE_2);
        for (n, (is_remote, icon, name)) in [(false, "home", t("fm.thisDevice")), (true, "monitor", label)].into_iter().enumerate() {
            let half = (w - 6.0) / 2.0;
            let r = Rect::from_min_size(Pos2::new(sw.left() + 3.0 + n as f32 * half, sw.top() + 3.0), Vec2::new(half, 34.0));
            let resp = ui.interact(r, Id::unique(("fm-side-btn", key, n)), Sense::click());
            let on = remote == is_remote;
            if on {
                ui.painter().add(th::shadow().as_shape(r, CornerRadius::same(8)));
                ui.painter().rect_filled(r, 8.0, th::SURFACE);
            }
            let col = if on { th::ACCENT } else { th::MUTED };
            let mut job = egui::text::LayoutJob::simple_singleline(name, th::font(13.0, if on { W::Semibold } else { W::Regular }), col);
            job.wrap = egui::text::TextWrapping::truncate_at_width(half - 44.0);
            let g = ui.painter().layout_job(job);
            let x = r.center().x - (g.size().x + 22.0) / 2.0;
            self.icons.paint(ui, icon, Rect::from_min_size(Pos2::new(x, r.center().y - 8.0), Vec2::splat(16.0)), col);
            ui.painter().galley(Pos2::new(x + 22.0, r.center().y - g.size().y / 2.0), g, col);
            if resp.clicked() {
                remote = is_remote;
                ctx.data_mut(|d| d.insert_temp(side_id, remote));
            }
        }

        let tab = &self.core.tabs[i];
        let p = if remote { &tab.fm.remote } else { &tab.fm.local };
        let other = if remote { &tab.fm.local } else { &tab.fm.remote };
        let nsel = p.selected.len();
        let mut push = |a: FmAct| acts.push((remote, a));

        if remote && !allowed {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                self.icons.show(ui, "shield", 32.0, th::MUTED);
                ui.add_space(8.0);
                ui.label(text(t("fm.noPerm"), 13.5, W::Regular, th::MUTED));
            });
        } else {
            // the phone's own files need "All files access"
            #[cfg(target_os = "android")]
            if !remote && !crate::android::files_access() {
                egui::Frame::new().fill(th::ACCENT_SOFT).corner_radius(10).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(text(t("fm.access"), 12.5, W::Regular, th::TEXT));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if th::btn(ui, &t("fm.accessBtn"), Btn::Primary).clicked() {
                            crate::android::open_files_access();
                        }
                        if th::btn(ui, &t("fm.refresh"), Btn::Normal).clicked() {
                            push(FmAct::Open(crate::fm::home().to_string_lossy().to_string()));
                        }
                    });
                });
            }
            // where we are: up, the folder (its path under it), the places, refresh
            ui.horizontal(|ui| {
                ui.set_height(44.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                let up = ui.add_enabled_ui(p.parent.is_some(), |ui| th::tbbtn(ui, &self.icons, "up", false, false, &t("fm.up"))).inner;
                if up.clicked() {
                    if let Some(par) = &p.parent {
                        push(FmAct::Open(par.clone()));
                    }
                }
                let right_w = 2.0 * 34.0 + 6.0;
                let tw = (ui.available_width() - right_w - 6.0).max(40.0);
                let folder = p.path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(&p.path).to_string();
                let (tr, _) = ui.allocate_exact_size(Vec2::new(tw, 40.0), Sense::hover());
                let mut job = egui::text::LayoutJob::simple_singleline(folder, th::font(14.5, W::Semibold), th::TEXT);
                job.wrap = egui::text::TextWrapping::truncate_at_width(tw);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(tr.left(), tr.top() + 2.0), g, th::TEXT);
                // the path, its end kept when it is too long
                let path = &p.path;
                let max_chars = (tw / 6.5) as usize;
                let shown = if path.chars().count() > max_chars { format!("…{}", path.chars().rev().take(max_chars.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { path.clone() };
                ui.painter().text(Pos2::new(tr.left(), tr.bottom() - 2.0), Align2::LEFT_BOTTOM, shown, th::font(11.5, W::Regular), th::MUTED);
                let places = th::tbbtn(ui, &self.icons, "folder", false, false, &t("fm.places"));
                egui::Popup::menu(&places).show(|ui| {
                    for r in &p.roots {
                        if ui.add(egui::Button::new(text(r, 13.5, W::Regular, th::TEXT)).frame(false).min_size(Vec2::new(220.0, 36.0))).clicked() {
                            push(FmAct::Open(r.clone()));
                        }
                    }
                });
                if p.loading {
                    ui.add(egui::Spinner::new().size(16.0).color(th::MUTED));
                } else if th::tbbtn(ui, &self.icons, "refresh", false, false, &t("fm.refresh")).clicked() {
                    push(FmAct::Refresh);
                }
            });
            if !p.error.is_empty() {
                ui.label(text(&p.error, 12.5, W::Regular, th::DANGER));
            }

            // the copies (newest two) and the actions take the bottom
            let shown_tr: Vec<&crate::fm::Transfer> = tab.fm.transfers.iter().rev().take(2).collect();
            let tr_h = if shown_tr.is_empty() { 0.0 } else { 34.0 + 40.0 * shown_tr.len() as f32 };
            let bar_h = if nsel > 0 { 104.0 } else { 52.0 };
            let list_h = (ui.available_height() - tr_h - bar_h - 8.0).max(80.0);
            const ROW: f32 = 58.0;
            egui::ScrollArea::vertical().id_salt(("fm-mlist", key, remote)).max_height(list_h).auto_shrink([false, false]).show_rows(ui, ROW, p.entries.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for n in range {
                    let e = &p.entries[n];
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
                    let sel = p.selected.contains(&e.n);
                    if sel {
                        ui.painter().rect_filled(r, 8.0, th::ACCENT_SOFT);
                    } else if resp.is_pointer_button_down_on() {
                        ui.painter().rect_filled(r, 8.0, th::SURFACE_2);
                    }
                    let icon = Rect::from_center_size(Pos2::new(r.left() + 22.0, r.center().y), Vec2::splat(26.0));
                    self.icons.paint(ui, if e.d { "folder" } else { "doc" }, icon, if e.d { th::WARN } else { th::MUTED });
                    let x0 = r.left() + 46.0;
                    let name_w = (r.width() - 46.0 - if nsel > 0 { 40.0 } else { 12.0 }).max(30.0);
                    let mut job = egui::text::LayoutJob::simple_singleline(e.n.clone(), th::font(14.5, W::Regular), th::TEXT);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
                    let g = ui.painter().layout_job(job);
                    ui.painter().galley(Pos2::new(x0, r.center().y - g.size().y - 1.0), g, th::TEXT);
                    let date = (e.m > 0).then(|| chrono::DateTime::from_timestamp_millis(e.m).map(|d| d.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default());
                    let what = if e.d { t("fm.folderItems") } else { crate::fm::human_size(e.s) };
                    let sub = match date {
                        Some(d) => format!("{what} · {d}"),
                        None => what,
                    };
                    ui.painter().text(Pos2::new(x0, r.center().y + 3.0), Align2::LEFT_TOP, sub, th::font(12.0, W::Regular), th::MUTED);
                    // selecting: a round check on the right
                    if nsel > 0 {
                        let c = Pos2::new(r.right() - 20.0, r.center().y);
                        if sel {
                            ui.painter().circle_filled(c, 10.0, th::ACCENT);
                            ui.painter().line_segment([c + Vec2::new(-4.5, 0.0), c + Vec2::new(-1.0, 3.5)], Stroke::new(2.0, Color32::WHITE));
                            ui.painter().line_segment([c + Vec2::new(-1.0, 3.5), c + Vec2::new(5.0, -3.5)], Stroke::new(2.0, Color32::WHITE));
                        } else {
                            ui.painter().circle_stroke(c, 10.0, Stroke::new(1.5, th::BORDER_STRONG));
                        }
                    }
                    ui.painter().hline(egui::Rangef::new(x0, r.right()), r.bottom() - 0.5, Stroke::new(1.0, th::BORDER));
                    // held for a moment: selecting starts (and the release is not a tap)
                    let long_id = Id::unique(("fm-long", key, remote));
                    if resp.is_pointer_button_down_on() {
                        let held = ui.input(|i| i.pointer.press_start_time().map(|t0| i.time - t0)).unwrap_or(0.0);
                        if held >= 0.45 && ui.data(|d| d.get_temp::<usize>(long_id)) != Some(n) {
                            ui.data_mut(|d| d.insert_temp(long_id, n));
                            if !sel {
                                push(FmAct::Click(n, true, false));
                            }
                        } else if held < 0.45 {
                            ui.ctx().request_repaint_after(Duration::from_millis(50));
                        }
                    }
                    let was_long = resp.clicked() && ui.data_mut(|d| d.remove_temp::<usize>(long_id)) == Some(n);
                    // a hold too long to count as a click ends without one
                    if !resp.clicked() && !ui.input(|i| i.pointer.any_down()) && ui.data(|d| d.get_temp::<usize>(long_id)) == Some(n) {
                        ui.data_mut(|d| d.remove_temp::<usize>(long_id));
                    }
                    if was_long {
                    } else if resp.secondary_clicked() {
                        if !sel {
                            push(FmAct::Click(n, true, false));
                        }
                    } else if resp.clicked() {
                        if nsel > 0 || !e.d {
                            push(FmAct::Click(n, true, false));
                        } else {
                            push(FmAct::Open(p.join(&e.n)));
                        }
                    }
                }
            });
            if p.entries.is_empty() && !p.loading && p.error.is_empty() {
                ui.add_space(-list_h + 30.0);
                ui.vertical_centered(|ui| ui.label(text(t("fm.empty"), 13.0, W::Regular, th::MUTED)));
                ui.add_space(list_h - 60.0);
            }

            // the copies
            if !shown_tr.is_empty() {
                let (r, _) = ui.allocate_exact_size(Vec2::new(w, tr_h), Sense::hover());
                ui.painter().rect(r, 10.0, th::SURFACE, Stroke::new(1.0, th::BORDER), StrokeKind::Inside);
                ui.painter().text(Pos2::new(r.left() + 12.0, r.top() + 17.0), Align2::LEFT_CENTER, t("fm.transfers"), th::font(12.5, W::Semibold), th::TEXT);
                if tab.fm.transfers.iter().any(|t| t.finished) {
                    let cr = Rect::from_min_size(Pos2::new(r.right() - 90.0, r.top() + 4.0), Vec2::new(84.0, 26.0));
                    let resp = ui.interact(cr, Id::unique(("fm-mclear", key)), Sense::click());
                    ui.painter().text(Pos2::new(cr.right() - 6.0, cr.center().y), Align2::RIGHT_CENTER, t("fm.clear"), th::font(12.5, W::Regular), th::ACCENT);
                    if resp.clicked() {
                        acts.push((false, FmAct::ClearDone));
                    }
                }
                for (k, tr) in shown_tr.iter().enumerate() {
                    let row = Rect::from_min_size(Pos2::new(r.left() + 12.0, r.top() + 34.0 + k as f32 * 40.0), Vec2::new(r.width() - 24.0, 40.0));
                    self.icons.paint(ui, if tr.upload { "arrow" } else { "back" }, Rect::from_min_size(Pos2::new(row.left(), row.top() + 4.0), Vec2::splat(15.0)), th::MUTED);
                    let frac = if tr.total > 0 { (tr.done as f32 / tr.total as f32).clamp(0.0, 1.0) } else if tr.finished { 1.0 } else { 0.0 };
                    let (fill, status) = if !tr.error.is_empty() {
                        (th::DANGER, tr.error.clone())
                    } else if tr.cancelled {
                        (th::MUTED, t("fm.cancelled"))
                    } else if tr.finished {
                        (th::OK, t("fm.done"))
                    } else {
                        (th::ACCENT, format!("{}%", (frac * 100.0) as u32))
                    };
                    let name_w = row.width() - 22.0 - 80.0 - if tr.finished { 0.0 } else { 30.0 };
                    let mut job = egui::text::LayoutJob::simple_singleline(tr.name.clone(), th::font(12.5, W::Regular), th::TEXT);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
                    let g = ui.painter().layout_job(job);
                    ui.painter().galley(Pos2::new(row.left() + 22.0, row.top() + 2.0), g, th::TEXT);
                    let st_right = row.right() - if tr.finished { 0.0 } else { 30.0 };
                    ui.painter().text(Pos2::new(st_right, row.top() + 2.0), Align2::RIGHT_TOP, status, th::font(12.0, W::Regular), fill);
                    let bar = Rect::from_min_size(Pos2::new(row.left() + 22.0, row.top() + 24.0), Vec2::new(st_right - row.left() - 22.0, 5.0));
                    ui.painter().rect_filled(bar, 2.5, th::BORDER);
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, 5.0)), 2.5, fill);
                    if !tr.finished {
                        let b = Rect::from_center_size(Pos2::new(row.right() - 10.0, row.top() + 14.0), Vec2::splat(28.0));
                        let resp = ui.interact(b, Id::unique(("fm-mx", &tr.id)), Sense::click());
                        self.icons.paint(ui, "x", b.shrink(7.0), th::MUTED);
                        if resp.clicked() {
                            acts.push((false, FmAct::Cancel(tr.id.clone())));
                        }
                    }
                }
            }

            // the actions
            if nsel > 0 {
                ui.horizontal(|ui| {
                    ui.set_height(40.0);
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if th::tbbtn(ui, &self.icons, "x", false, false, &t("fm.cancelSel")).clicked() {
                        acts.push((remote, FmAct::SelectNone));
                    }
                    ui.label(text(tf("fm.selected", &[("n", &nsel.to_string())]), 13.5, W::Semibold, th::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if th::tbbtn(ui, &self.icons, "trash", false, true, &t("fm.delete")).clicked() {
                            acts.push((remote, FmAct::Delete));
                        }
                        if nsel == 1 && th::tbbtn(ui, &self.icons, "edit", false, false, &t("fm.rename")).clicked() {
                            acts.push((remote, FmAct::Rename));
                        }
                        if nsel < p.entries.len() && ui.add(egui::Button::new(text(t("fm.selectAll"), 13.0, W::Regular, th::ACCENT)).frame(false)).clicked() {
                            acts.push((remote, FmAct::SelectAll));
                        }
                    });
                });
                let ok = allowed && !other.path.is_empty();
                let label = if remote { t("fm.toHere") } else { t("fm.toRemote") };
                if ui.add_enabled_ui(ok, |ui| th::btn_sized(ui, &label, Btn::Primary, Some(w))).inner.clicked() {
                    acts.push((remote, FmAct::Copy));
                    acts.push((remote, FmAct::SelectNone));
                }
            } else {
                ui.horizontal(|ui| {
                    ui.set_height(44.0);
                    if th::btn(ui, &format!("+  {}", t("fm.newFolder")), Btn::Normal).clicked() {
                        acts.push((remote, FmAct::Mkdir));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(text(tf("fm.items", &[("n", &p.entries.len().to_string())]), 12.5, W::Regular, th::MUTED));
                    });
                });
            }
        }
        for (remote, a) in acts {
            self.fm_apply(&ctx, key, remote, a);
        }
        if self.core.tabs.get(i).is_some_and(|t| t.fm.busy()) {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        self.fm_dialog(&ctx);
    }

    /// One side: header, path, tools, the listing and the copy button.
    #[allow(clippy::too_many_arguments)]
    fn fm_pane(&self, ui: &mut Ui, rect: Rect, i: usize, remote: bool, title: &str, can_copy: bool, acts: &mut Vec<(bool, FmAct)>) {
        let tab = &self.core.tabs[i];
        let key = tab.key;
        let p = if remote { &tab.fm.remote } else { &tab.fm.local };
        let focus_id = Id::unique(("fm-focus", key));
        let focused = ui.ctx().data(|d| d.get_temp::<bool>(focus_id)).unwrap_or(false) == remote;
        ui.painter().rect(rect, th::RADIUS, th::SURFACE, Stroke::new(1.0, if focused { th::BORDER_STRONG } else { th::BORDER }), StrokeKind::Inside);
        let mut push = |a: FmAct| acts.push((remote, a));
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(Vec2::new(12.0, 10.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.spacing_mut().item_spacing.y = 6.0;
        let nsel = p.selected.len();
        // header
        ui.horizontal(|ui| {
            ui.set_height(30.0);
            ui.spacing_mut().item_spacing.x = 8.0;
            self.icons.show(ui, if remote { "monitor" } else { "home" }, 18.0, th::MUTED);
            ui.label(text(title, 13.5, W::Semibold, th::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if th::iconbtn(ui, &self.icons, "refresh", &t("fm.refresh")).clicked() {
                    push(FmAct::Refresh);
                }
                if p.loading {
                    ui.add(egui::Spinner::new().size(14.0).color(th::MUTED));
                }
            });
        });
        // path: up, the roots (home, drives), the folder (editable)
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let up = ui.add_enabled_ui(p.parent.is_some(), |ui| th::iconbtn(ui, &self.icons, "up", &t("fm.up"))).inner;
            if up.clicked() {
                if let Some(par) = &p.parent {
                    push(FmAct::Open(par.clone()));
                }
            }
            let cur = p.roots.iter().filter(|r| p.path.starts_with(r.as_str())).max_by_key(|r| r.len()).cloned().unwrap_or_default();
            let short = |r: &str| if r.len() > 14 { format!("…{}", &r[r.len() - r.chars().rev().take(12).map(char::len_utf8).sum::<usize>()..]) } else { r.to_string() };
            egui::ComboBox::from_id_salt(("fm-roots", key, remote))
                .width(110.0)
                .selected_text(text(short(&cur), 12.5, W::Regular, th::TEXT))
                .show_ui(ui, |ui| {
                    for r in &p.roots {
                        if ui.selectable_label(*r == cur, r.as_str()).clicked() {
                            push(FmAct::Open(r.clone()));
                        }
                    }
                });
            let buf_id = Id::unique(("fm-path", key, remote));
            let edit_id = buf_id.with("edit");
            let editing = ui.memory(|m| m.has_focus(edit_id));
            let mut buf = if editing { ui.data(|d| d.get_temp::<String>(buf_id)).unwrap_or_else(|| p.path.clone()) } else { p.path.clone() };
            let r = ui.add(egui::TextEdit::singleline(&mut buf).id(edit_id).font(th::font(12.5, W::Regular)).desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && buf.trim() != p.path {
                push(FmAct::Open(buf.trim().to_string()));
            }
            ui.data_mut(|d| d.insert_temp(buf_id, buf));
        });
        // tools
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if th::iconbtn(ui, &self.icons, "plus", &t("fm.newFolder")).clicked() {
                push(FmAct::Mkdir);
            }
            if ui.add_enabled_ui(nsel == 1, |ui| th::iconbtn(ui, &self.icons, "edit", &t("fm.rename"))).inner.clicked() {
                push(FmAct::Rename);
            }
            if ui.add_enabled_ui(nsel > 0, |ui| th::iconbtn(ui, &self.icons, "trash", &t("fm.delete"))).inner.clicked() {
                push(FmAct::Delete);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let s = if nsel > 0 { tf("fm.selected", &[("n", &nsel.to_string())]) } else { tf("fm.items", &[("n", &p.entries.len().to_string())]) };
                ui.label(text(s, 12.0, W::Regular, th::MUTED));
            });
        });
        if !p.error.is_empty() {
            ui.label(text(&p.error, 12.0, W::Regular, th::DANGER));
        }
        // the listing
        let w = ui.available_width();
        let show_date = w > 360.0;
        let (size_w, date_w) = (78.0, if show_date { 118.0 } else { 0.0 });
        let (hr, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
        let hfont = th::font(11.5, W::Medium);
        ui.painter().text(Pos2::new(hr.left() + 30.0, hr.center().y), Align2::LEFT_CENTER, t("fm.name"), hfont.clone(), th::MUTED);
        ui.painter().text(Pos2::new(hr.right() - date_w - 10.0, hr.center().y), Align2::RIGHT_CENTER, t("fm.size"), hfont.clone(), th::MUTED);
        if show_date {
            ui.painter().text(Pos2::new(hr.right() - 4.0, hr.center().y), Align2::RIGHT_CENTER, t("fm.modified"), hfont, th::MUTED);
        }
        ui.painter().hline(hr.x_range(), hr.bottom(), Stroke::new(1.0, th::BORDER));
        let list_h = (ui.available_height() - 44.0).max(40.0);
        let list_top = ui.cursor().top();
        egui::ScrollArea::vertical().id_salt(("fm-list", key, remote)).max_height(list_h).auto_shrink([false, false]).show_rows(ui, FM_ROW, p.entries.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for n in range {
                let e = &p.entries[n];
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), FM_ROW), Sense::click());
                let sel = p.selected.contains(&e.n);
                if sel {
                    ui.painter().rect_filled(r, 5.0, th::ACCENT_SOFT);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 5.0, th::SURFACE_2);
                }
                let icon = Rect::from_center_size(Pos2::new(r.left() + 15.0, r.center().y), Vec2::splat(17.0));
                self.icons.paint(ui, if e.d { "folder" } else { "doc" }, icon, if e.d { th::WARN } else { th::MUTED });
                let name_w = (r.width() - 30.0 - size_w - date_w - 16.0).max(20.0);
                let mut job = egui::text::LayoutJob::simple_singleline(e.n.clone(), th::font(13.0, W::Regular), th::TEXT);
                job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(r.left() + 30.0, r.center().y - g.size().y / 2.0), g, th::TEXT);
                if !e.d {
                    ui.painter().text(Pos2::new(r.right() - date_w - 10.0, r.center().y), Align2::RIGHT_CENTER, crate::fm::human_size(e.s), th::font(12.0, W::Regular), th::MUTED);
                }
                if show_date && e.m > 0 {
                    let d = chrono::DateTime::from_timestamp_millis(e.m).map(|d| d.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default();
                    ui.painter().text(Pos2::new(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, d, th::font(12.0, W::Regular), th::MUTED);
                }
                if resp.double_clicked() {
                    if e.d {
                        push(FmAct::Open(p.join(&e.n)));
                    }
                } else if resp.clicked() {
                    let m = ui.input(|i| i.modifiers);
                    push(FmAct::Click(n, m.command, m.shift));
                }
            }
        });
        if p.entries.is_empty() && !p.loading && p.error.is_empty() {
            let c = Pos2::new(rect.center().x, list_top + 40.0);
            ui.painter().text(c, Align2::CENTER_CENTER, t("fm.empty"), th::font(12.5, W::Regular), th::MUTED);
        }
        // footer: copy to the other side
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if remote { format!("← {}", t("fm.download")) } else { format!("{} →", t("fm.upload")) };
            let (other, _) = (if remote { &tab.fm.local } else { &tab.fm.remote }, ());
            let ok = nsel > 0 && can_copy && !other.path.is_empty();
            if remote {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(ok, |ui| th::btn(ui, &label, Btn::Primary)).inner.clicked() {
                        push(FmAct::Copy);
                    }
                });
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(ok, |ui| th::btn(ui, &label, Btn::Primary)).inner.clicked() {
                        push(FmAct::Copy);
                    }
                });
            }
        });
        // keys, for the pane last clicked (not while typing somewhere)
        if focused && ui.ctx().memory(|m| m.focused().is_none()) && self.core.capture_tab != Some(key) {
            let (del, enter, back, all, f2, f5) = ui.input_mut(|i| {
                (
                    i.key_pressed(egui::Key::Delete),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Backspace),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                    i.key_pressed(egui::Key::F2),
                    i.key_pressed(egui::Key::F5),
                )
            });
            if del && nsel > 0 {
                push(FmAct::Delete);
            }
            if enter && nsel == 1 {
                if let Some(e) = p.entries.iter().find(|e| e.d && p.selected[0] == e.n) {
                    push(FmAct::Open(p.join(&e.n)));
                }
            }
            if back {
                if let Some(par) = &p.parent {
                    push(FmAct::Open(par.clone()));
                }
            }
            if all {
                push(FmAct::SelectAll);
            }
            if f2 && nsel == 1 {
                push(FmAct::Rename);
            }
            if f5 {
                push(FmAct::Refresh);
            }
        }
    }

    /// The copies, newest first, with progress and a stop button.
    fn fm_transfers(&self, ui: &mut Ui, rect: Rect, i: usize, acts: &mut Vec<(bool, FmAct)>) {
        let f = &self.core.tabs[i].fm;
        ui.painter().rect(rect, th::RADIUS, th::SURFACE, Stroke::new(1.0, th::BORDER), StrokeKind::Inside);
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(Vec2::new(12.0, 8.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut ui;
        ui.horizontal(|ui| {
            ui.set_height(28.0);
            ui.label(text(t("fm.transfers"), 13.0, W::Semibold, th::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if f.transfers.iter().any(|t| t.finished) && ui.add(egui::Button::new(text(t("fm.clear"), 12.5, W::Regular, th::ACCENT)).frame(false)).clicked() {
                    acts.push((false, FmAct::ClearDone));
                }
            });
        });
        egui::ScrollArea::vertical().id_salt(("fm-tr", self.core.tabs[i].key)).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for tr in f.transfers.iter().rev() {
                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), FM_TR_ROW), Sense::hover());
                let icon = Rect::from_center_size(Pos2::new(r.left() + 10.0, r.center().y), Vec2::splat(16.0));
                self.icons.paint(ui, if tr.upload { "arrow" } else { "back" }, icon, th::MUTED);
                let x0 = r.left() + 28.0;
                let bar_w = (r.width() * 0.35).clamp(80.0, 260.0);
                let status_w = 170.0;
                let name_w = (r.width() - 28.0 - bar_w - status_w - 50.0).max(30.0);
                let mut job = egui::text::LayoutJob::simple_singleline(tr.name.clone(), th::font(12.5, W::Regular), th::TEXT);
                job.wrap = egui::text::TextWrapping::truncate_at_width(name_w);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(x0, r.center().y - g.size().y / 2.0), g, th::TEXT);
                let frac = if tr.total > 0 { (tr.done as f32 / tr.total as f32).clamp(0.0, 1.0) } else if tr.finished { 1.0 } else { 0.0 };
                let bar = Rect::from_min_size(Pos2::new(x0 + name_w + 12.0, r.center().y - 3.0), Vec2::new(bar_w, 6.0));
                ui.painter().rect_filled(bar, 3.0, th::BORDER);
                let (fill, status) = if !tr.error.is_empty() {
                    (th::DANGER, tr.error.clone())
                } else if tr.cancelled {
                    (th::MUTED, t("fm.cancelled"))
                } else if tr.finished {
                    (th::OK, format!("{} · {}", t("fm.done"), crate::fm::human_size(tr.total)))
                } else {
                    (th::ACCENT, format!("{} / {} · {}%", crate::fm::human_size(tr.done), crate::fm::human_size(tr.total), (frac * 100.0) as u32))
                };
                ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(bar_w * frac, 6.0)), 3.0, fill);
                let mut job = egui::text::LayoutJob::simple_singleline(status, th::font(12.0, W::Regular), if tr.error.is_empty() { th::MUTED } else { th::DANGER });
                job.wrap = egui::text::TextWrapping::truncate_at_width(status_w);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(bar.right() + 12.0, r.center().y - g.size().y / 2.0), g, th::MUTED);
                if !tr.finished {
                    let b = Rect::from_center_size(Pos2::new(r.right() - 16.0, r.center().y), Vec2::splat(28.0));
                    let resp = ui.interact(b, Id::unique(("fm-x", &tr.id)), Sense::click()).on_hover_text(t("btn.cancel"));
                    if resp.hovered() {
                        ui.painter().rect_filled(b, 6.0, th::SURFACE_2);
                    }
                    self.icons.paint(ui, "x", b.shrink(7.0), if resp.hovered() { th::TEXT } else { th::MUTED });
                    if resp.clicked() {
                        acts.push((false, FmAct::Cancel(tr.id.clone())));
                    }
                }
            }
        });
    }

    fn fm_apply(&mut self, ctx: &egui::Context, key: u32, remote: bool, a: FmAct) {
        let focus_id = Id::unique(("fm-focus", key));
        let Some(tab) = self.core.tab(key) else { return };
        let f = &mut tab.fm;
        let pane = if remote { &mut f.remote } else { &mut f.local };
        let names = pane.selected.clone();
        match a {
            FmAct::Open(path) => {
                if remote {
                    self.core.fm_ls(key, &path);
                } else {
                    self.core.fm_ls_local(key, &path);
                }
            }
            FmAct::Click(n, toggle, range) => pane.click(n, toggle, range),
            FmAct::SelectAll => pane.selected = pane.entries.iter().map(|e| e.n.clone()).collect(),
            FmAct::SelectNone => pane.selected.clear(),
            FmAct::Refresh => {
                let path = pane.path.clone();
                return self.fm_apply(ctx, key, remote, FmAct::Open(path));
            }
            FmAct::Copy => self.core.fm_copy(key, !remote, None),
            FmAct::Mkdir => self.fm_dialog = Some(FmDialog::Mkdir { key, remote, name: String::new() }),
            FmAct::Rename => {
                if let [old] = names.as_slice() {
                    self.fm_dialog = Some(FmDialog::Rename { key, remote, old: old.clone(), name: old.clone() });
                }
            }
            FmAct::Delete => {
                if !names.is_empty() {
                    self.fm_dialog = Some(FmDialog::Delete { key, remote, names });
                }
            }
            FmAct::Cancel(id) => return self.core.fm_cancel(key, &id),
            FmAct::ClearDone => return f.transfers.retain(|t| !t.finished),
        }
        ctx.data_mut(|d| d.insert_temp(focus_id, remote));
    }

    /// New folder / rename / delete, over the file manager.
    fn fm_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.fm_dialog.as_mut() else { return };
        let mut close = false;
        let mut submit = false;
        let modal = dialog_modal(ctx, "fm-dialog").frame(egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).shadow(th::shadow_lg()).inner_margin(Margin::same(22))).show(ctx, |ui| {
            ui.set_width(dialog_width(ui.ctx(), 340.0));
            let (title, name) = match d {
                FmDialog::Mkdir { name, .. } => (t("fm.newFolder"), Some(name)),
                FmDialog::Rename { name, .. } => (t("fm.rename"), Some(name)),
                FmDialog::Delete { .. } => (t("fm.delete"), None),
            };
            th::h3(ui, &title);
            ui.add_space(6.0);
            let mut valid = true;
            if let Some(name) = name {
                field(ui, &t("fm.nameField"), |ui| {
                    let r = ui.add(egui::TextEdit::singleline(name).frame(egui::Frame::NONE).font(th::font(13.5, W::Regular)).desired_width(f32::INFINITY));
                    if !ui.ctx().memory(|m| m.focused().is_some()) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                });
                valid = crate::fm::valid_name(name);
                if !valid && !name.trim().is_empty() {
                    ui.label(text(t("fm.badName"), 12.0, W::Regular, th::DANGER));
                    ui.add_space(6.0);
                }
            } else if let FmDialog::Delete { names, .. } = d {
                let what = crate::fm::summary(names, |n| tf("fm.items", &[("n", &n.to_string())]));
                ui.label(text(tf("fm.deleteAsk", &[("what", &what)]), 13.0, W::Regular, th::TEXT));
                ui.add_space(12.0);
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let bw = (ui.available_width() - 8.0) / 2.0;
                if th::btn_sized(ui, &t("btn.cancel"), Btn::Normal, Some(bw)).clicked() {
                    close = true;
                }
                let (label, kind) = match d {
                    FmDialog::Mkdir { .. } => (t("fm.create"), Btn::Primary),
                    FmDialog::Rename { .. } => (t("fm.rename"), Btn::Primary),
                    FmDialog::Delete { .. } => (t("fm.delete"), Btn::Danger),
                };
                if th::btn_sized(ui, &label, kind, Some(bw)).clicked() {
                    submit = true;
                }
            });
            if !valid {
                submit = false;
            }
        });
        if modal.should_close() {
            close = true;
        }
        if submit {
            match self.fm_dialog.take() {
                Some(FmDialog::Mkdir { key, remote, name }) => self.core.fm_mkdir(key, remote, &name),
                Some(FmDialog::Rename { key, remote, old, name }) if name.trim() != old => self.core.fm_rename(key, remote, &old, &name),
                Some(FmDialog::Delete { key, remote, names }) => self.core.fm_delete(key, remote, &names),
                _ => {}
            }
        } else if close {
            self.fm_dialog = None;
        }
    }

    // ================================================================ host side ===

    /// .accept-back + .accept
    fn request_window(&mut self, ctx: &egui::Context) {
        let Some(req) = self.core.pending.clone() else {
            self.req_perms = self.core.store.perms;
            return;
        };
        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("accept-back")).fixed_pos(screen.min).order(egui::Order::Foreground).show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(16, 24, 40, 89));
        });
        // a file transfer or terminal request: what it needs is ticked (and can be unticked)
        if self.req_seen != Some(req.at) {
            self.req_seen = Some(req.at);
            match req.mode {
                crate::core::SessMode::Files => self.req_perms.files = true,
                crate::core::SessMode::Terminal => self.req_perms.terminal = true,
                crate::core::SessMode::Desktop => {}
            }
        }
        let left = 60u64.saturating_sub(req.at.elapsed().as_secs());
        let mut decision = None;
        let w = 400.0_f32.min(screen.width() - 32.0);
        egui::Area::new(Id::unique("accept")).order(egui::Order::Tooltip).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).shadow(th::shadow_lg()).inner_margin(22).show(ui, |ui| {
                ui.set_width(w - 44.0);
                self.accept_head(ui, &req.name, &req.os, &req.from, None);
                // AnyDesk says what kind of session it is: a file transfer has no screen
                if req.mode != crate::core::SessMode::Desktop {
                    ui.add_space(12.0);
                    egui::Frame::new().fill(th::ACCENT_SOFT).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            self.icons.show(ui, mode_icon(req.mode), 16.0, th::ACCENT);
                            let k = if req.mode == crate::core::SessMode::Files { "req.wantsFiles" } else { "req.wantsTerm" };
                            ui.label(text(t(k), 13.0, W::Medium, th::TEXT));
                        });
                    });
                }
                ui.add_space(16.0);
                ui.label(text(t("acc.allow").to_uppercase(), 11.5, W::Semibold, th::MUTED));
                ui.add_space(6.0);
                perm_toggles(ui, &self.icons, &mut self.req_perms);
                let live = self.core.hosts.len();
                let q = self.core.queue.len();
                let mut parts = vec![tf("acc.left", &[("s", &left.to_string())])];
                if live > 0 {
                    parts.push(tf("acc.live", &[("n", &live.to_string())]));
                }
                if q > 0 {
                    parts.push(tf("acc.queue", &[("n", &q.to_string())]));
                }
                ui.add_space(10.0);
                th::hint(ui, &parts.join(" · "));
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let bw = (ui.available_width() - 8.0) / 2.0;
                    if th::btn_sized(ui, &t("acc.reject"), Btn::Normal, Some(bw)).clicked() {
                        decision = Some(false);
                    }
                    if th::btn_sized(ui, &t("acc.accept"), Btn::Primary, Some(bw)).clicked() {
                        decision = Some(true);
                    }
                });
            });
        });
        if let Some(accept) = decision {
            self.core.decide(accept, self.req_perms);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// .accept-head: avatar, name, "os · from"
    fn accept_head(&self, ui: &mut Ui, name: &str, os: &str, from: &str, live: Option<(Color32, String)>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let (r, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
            let (bg, fg) = match &live {
                Some((c, _)) => (th::mix(*c, th::SURFACE, 0.14), *c),
                None => (th::ACCENT_SOFT, th::ACCENT),
            };
            ui.painter().rect_filled(r, 10.0, bg);
            self.icons.paint(ui, "monitor", Rect::from_center_size(r.center(), Vec2::splat(24.0)), fg);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), th::font(16.0, W::Semibold), th::TEXT);
                job.wrap = egui::text::TextWrapping::truncate_at_width((ui.available_width() - 4.0).max(40.0));
                ui.label(job);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    match &live {
                        Some((_, state)) => {
                            th::pulse(ui);
                            ui.label(text(state, 12.5, W::Regular, th::MUTED));
                        }
                        None => {
                            ui.label(text(crate::core::os_name(os), 12.5, W::Regular, th::MUTED));
                        }
                    }
                    if !from.is_empty() {
                        ui.label(RichText::new(format!("· {}", rv::format_id(from))).font(th::mono(12.0, W::Regular)).color(th::MUTED));
                    }
                });
            });
        });
    }

    /// .host-stack: one .host-panel per viewer of this screen.
    fn host_stack(&mut self, ctx: &egui::Context) {
        if self.core.hosts.is_empty() {
            self.host_live_at.clear();
            self.auto_folded.clear();
            return;
        }
        // a few seconds after a session goes live its card folds to a bar
        let now = Instant::now();
        let live: Vec<u32> = self.core.hosts.iter().filter(|h| h.state == HostState::Live && h.auth.is_none()).map(|h| h.key).collect();
        self.host_live_at.retain(|k, _| live.contains(k));
        for k in live {
            let at = *self.host_live_at.entry(k).or_insert(now);
            if now.duration_since(at) > Duration::from_secs(4) {
                if self.auto_folded.insert(k) {
                    self.collapsed.insert(k);
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
        // a chat is open (this side's host chat, or the chat of the session in front)
        let viewer_chat = self.core.active.and_then(|k| self.core.tabs.iter().find(|t| t.key == k)).is_some_and(|t| t.chat.open);
        let chat = self.host_chat.is_some() || viewer_chat;
        let narrow = compact(ctx);
        // a phone: the chat is the whole screen, and typing needs the keyboard's room
        if narrow && (chat || ctx.egui_wants_keyboard_input()) {
            return;
        }
        let right = if narrow { 12.0 } else { 18.0 + if chat { CHAT_W } else { 0.0 } };
        let width = if narrow { (ctx.content_rect().width() - 24.0).min(360.0) } else { 360.0 };
        #[cfg(mobile)]
        let bottom = 18.0 + crate::phone::insets().1 + if self.core.active.is_some() { 84.0 } else { 0.0 };
        #[cfg(not(mobile))]
        let bottom = 18.0;
        let mut actions: Vec<(u32, &'static str, bool, bool)> = Vec::new();
        let mut perm_change = None;
        let hosts: Vec<u32> = self.core.hosts.iter().map(|h| h.key).collect();
        egui::Area::new(Id::unique("host-stack")).anchor(Align2::RIGHT_BOTTOM, Vec2::new(-right, -bottom)).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_width(width);
            for key in hosts {
                let Some(h) = self.core.hosts.iter().find(|h| h.key == key) else { continue };
                let color = Color32::from_rgb(h.color[0], h.color[1], h.color[2]);
                let live = h.state == HostState::Live;
                let collapsed = self.collapsed.contains(&key);
                let chat_active = self.host_chat == Some(key);
                let (name, from, sas, stats, remote_mic, unread, mic_on) = (h.req.name.clone(), h.req.from.clone(), h.sas.clone(), h.stats.clone(), h.remote_mic, h.chat.unread, h.mic.is_some());
                let (cam_on, can_cam) = (h.cams.mine.is_some(), h.req.cam);
                let mut perms = h.perms;
                let waiting = h.auth.is_some();
                let fr = egui::Frame::new()
                    .fill(th::SURFACE)
                    .stroke(Stroke::new(1.0, if chat_active { th::ACCENT } else { th::BORDER }))
                    .corner_radius(12)
                    .shadow(th::shadow_lg())
                    .inner_margin(Margin { left: 20, right: 16, top: 14, bottom: 14 })
                    .show(ui, |ui| {
                        ui.set_width(width - 36.0);
                        ui.horizontal(|ui| {
                            let state = if waiting { t("host.authWait") } else if live { t("host.connected") } else { t("host.connecting") };
                            ui.scope(|ui| {
                                ui.set_width(width - 36.0 - 40.0);
                                self.accept_head(ui, &name, "", &from, Some((if live { color } else { th::MUTED }, state)));
                            });
                            if th::iconbtn(ui, &self.icons, if collapsed { "expand" } else { "shrink" }, &t("tt.collapse")).clicked() {
                                actions.push((key, "collapse", false, false));
                            }
                        });
                        if !collapsed {
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label(text(t("sas"), 12.5, W::Regular, th::MUTED));
                                ui.label(mono(if sas.is_empty() { "— —".into() } else { sas.clone() }, 18.0, W::Semibold, th::OK, 1.5));
                            });
                            ui.add_space(8.0);
                            if !waiting && perm_toggles(ui, &self.icons, &mut perms) {
                                perm_change = Some((key, perms));
                            }
                            if remote_mic {
                                ui.horizontal(|ui| {
                                    self.icons.show(ui, "speaker", 18.0, th::OK);
                                    ui.label(text(t("host.remoteMic"), 12.5, W::Regular, th::OK));
                                });
                            }
                            if !stats.is_empty() {
                                ui.add_space(6.0);
                                ui.label(text(&stats, 12.0, W::Regular, th::MUTED));
                            }
                        }
                        ui.add_space(if collapsed { 8.0 } else { 12.0 });
                        let y = ui.cursor().top();
                        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, th::BORDER));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let r = th::tbbtn(ui, &self.icons, "chat", chat_active, false, &t("tt.chat"));
                            th::badge(ui, r.rect, unread);
                            if r.clicked() {
                                actions.push((key, "chat", false, false));
                            }
                            if th::tbbtn(ui, &self.icons, "mic", mic_on, false, &t(if mic_on { "tt.micOn" } else { "tt.mic" })).clicked() && live {
                                actions.push((key, "mic", !mic_on, false));
                            }
                            if can_cam && th::tbbtn(ui, &self.icons, "camera", cam_on, false, &t(if cam_on { "tt.camOn" } else { "tt.cam" })).clicked() && live {
                                actions.push((key, "camera", !cam_on, false));
                            }
                            if th::tbbtn(ui, &self.icons, "file", false, false, &t("tt.file")).clicked() && live {
                                actions.push((key, "file", false, false));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if th::btn(ui, &t("host.disconnect"), Btn::Danger).clicked() {
                                    actions.push((key, "close", false, false));
                                }
                            });
                        });
                    });
                // border-left: 4px solid var(--who)
                let r = fr.response.rect;
                ui.painter().rect_filled(Rect::from_min_size(r.min, Vec2::new(4.0, r.height())), CornerRadius { nw: 12, sw: 12, ne: 0, se: 0 }, color);
                ui.add_space(10.0);
            }
        });
        for (k, a, c, cb) in actions {
            match a {
                "collapse" => {
                    if !self.collapsed.remove(&k) {
                        self.collapsed.insert(k);
                    }
                }
                "chat" => {
                    let open = self.host_chat != Some(k);
                    for h in &mut self.core.hosts {
                        h.chat.open = open && h.key == k;
                        if h.key == k {
                            h.chat.unread = 0;
                        }
                    }
                    self.host_chat = open.then_some(k);
                }
                "mic" => self.core.set_mic(None, Some(k), c),
                "camera" => self.core.set_camera(None, Some(k), c),
                "file" => {
                    let sid = self.core.hosts.iter().find(|h| h.key == k).and_then(|h| h.sid);
                    #[cfg(not(mobile))]
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        self.core.send_files(sid, files);
                    }
                    #[cfg(mobile)]
                    let _ = sid;
                }
                "close" => self.core.close_host(k, true),
                _ => {
                    let _ = cb;
                }
            }
        }
        if let Some((k, p)) = perm_change {
            self.core.set_host_perms(k, p);
        }
    }

    /// Host only: a click-through window over the shared screen with one labelled marker
    /// per viewer where that viewer points / clicks / types (overlay.html of the web build).
    fn pointer_overlay(&mut self, ctx: &egui::Context) {
        const TITLE: &str = "ManyDesk overlay";
        let now = Instant::now();
        type Mark = (Color32, String, String, crate::core::Pointer);
        // One overlay window per display that has a viewer's pointer on it.
        type Line = (Color32, Vec<(f32, f32)>);
        let mut per_screen: Vec<(crate::media::capture::Screen, Vec<Mark>, Vec<Line>)> = Vec::new();
        for h in self.core.hosts.iter().filter(|h| h.state == HostState::Live && h.perms.control) {
            let Some(sc) = h.screen.clone() else { continue };
            let p = h.pointer.filter(|p| now.duration_since(p.at) < Duration::from_secs(15));
            if p.is_none() && h.strokes.is_empty() {
                continue;
            }
            let at = match per_screen.iter().position(|x| x.0.id == sc.id) {
                Some(n) => n,
                None => {
                    per_screen.push((sc, Vec::new(), Vec::new()));
                    per_screen.len() - 1
                }
            };
            if let Some(p) = p {
                let id = &h.req.from;
                per_screen[at].1.push((Color32::from_rgb(h.color[0], h.color[1], h.color[2]), h.req.name.clone(), id[id.len().saturating_sub(3)..].to_string(), p));
            }
            // the viewer's whiteboard
            for s in &h.strokes {
                per_screen[at].2.push((Color32::from_rgb(s.color[0], s.color[1], s.color[2]), s.pts.clone()));
            }
        }
        if per_screen.is_empty() || !OVERLAY.load(std::sync::atomic::Ordering::Relaxed) {
            self.overlay_hidden.clear();
            return;
        }
        self.overlay_hidden.retain(|id| per_screen.iter().any(|x| x.0.id == *id));
        let acts = [("down", t("ov.click")), ("scroll", t("ov.scroll")), ("key", t("ov.type"))];
        for (screen, marks, lines) in per_screen {
            let (x, y, w, h) = crate::input::overlay_rect(&screen);
            let title = format!("{TITLE} {}", screen.id);
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_position([x, y])
                .with_inner_size([w, h])
                .with_decorations(false)
                .with_transparent(true)
                .with_has_shadow(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_active(false)
                .with_mouse_passthrough(true)
                .with_window_level(egui::WindowLevel::AlwaysOnTop);
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("md-overlay", screen.id)), builder, |ui, _| {
                let p = ui.painter().clone();
                let area = ui.max_rect();
                for (c, pts) in &lines {
                    let pts: Vec<Pos2> = pts.iter().map(|(x, y)| Pos2::new(area.left() + x * area.width(), area.top() + y * area.height())).collect();
                    p.add(egui::Shape::line(pts, Stroke::new(3.5, *c)));
                }
                for (color, name, id, ptr) in &marks {
                    let age = now.duration_since(ptr.at).as_secs_f32();
                    // Fades when the viewer stops; comes back on the next action.
                    let alpha = if age < 2.5 { 1.0 } else { 0.35 * (1.0 - ((age - 13.0) / 2.0).clamp(0.0, 1.0)) };
                    let c = |a: f32| color.gamma_multiply(a * alpha);
                    let at = Pos2::new(ptr.x, ptr.y);
                    p.circle(at, 9.0, c(0.18), Stroke::new(2.5, c(1.0)));
                    if let Some(t0) = ptr.click {
                        let k = now.duration_since(t0).as_secs_f32() / 0.6;
                        if k < 1.0 {
                            p.circle_stroke(at, 18.0 * (0.3 + 1.1 * k), Stroke::new(3.0, color.gamma_multiply(1.0 - k)));
                        }
                    }
                    let act = if age < 2.5 { acts.iter().find(|a| a.0 == ptr.kind).map(|a| format!("· {}", a.1)).unwrap_or_default() } else { String::new() };
                    let white = Color32::WHITE.gamma_multiply(alpha);
                    let mut job = egui::text::LayoutJob::default();
                    job.append(name, 0.0, egui::TextFormat { font_id: th::font(12.0, W::Semibold), color: white, ..Default::default() });
                    job.append(id, 6.0, egui::TextFormat { font_id: th::mono(11.0, W::Regular), color: white.gamma_multiply(0.85), ..Default::default() });
                    if !act.is_empty() {
                        job.append(&act, 6.0, egui::TextFormat { font_id: th::font(12.0, W::Medium), color: white.gamma_multiply(0.9), ..Default::default() });
                    }
                    let g = p.layout_job(job);
                    let tag = Rect::from_min_size(at + Vec2::new(14.0, 12.0), g.size() + Vec2::new(15.0, 6.0));
                    let cr = CornerRadius { nw: 4, ne: 10, sw: 10, se: 10 };
                    p.add(egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha((89.0 * alpha) as u8) }.as_shape(tag, cr));
                    p.rect_filled(tag, cr, c(1.0));
                    p.galley(tag.min + Vec2::new(7.0, 3.0), g, white);
                }
                if !self.overlay_hidden.contains(&screen.id) && crate::platform::exclude_from_capture(&title) {
                    self.overlay_hidden.push(screen.id);
                }
            });
        }
        ctx.request_repaint_after(Duration::from_millis(40));
    }

    fn host_chat_column(&mut self, ui: &mut Ui, key: u32) {
        let Some(i) = self.core.hosts.iter().position(|h| h.key == key) else { return };
        let name = self.core.hosts[i].req.name.clone();
        let mut send = false;
        let mut close = false;
        egui::Panel::right("host-chat").exact_size(chat_width(ui.ctx())).resizable(false).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
            (send, close) = chat_column(ui, &self.icons, &mut self.core.hosts[i].chat, &name);
        });
        if close {
            self.core.hosts[i].chat.open = false;
            self.host_chat = None;
        }
        if send {
            self.core.send_chat(None, Some(key));
        }
    }

    // ================================================================ toasts ===

    fn toasts(&mut self, ctx: &egui::Context) {
        if self.core.toasts.is_empty() {
            return;
        }
        let mut open = None;
        // A phone: Android's own toast look (AnyDesk mobile shows these) — a dark rounded
        // pill low on the screen, above the navigation bar and the session's menu button,
        // as wide as the text needs (long text wraps). A desktop: a bar under the title.
        let narrow = compact(ctx);
        let max_w = 520.0f32.min(ctx.content_rect().width() - 32.0);
        #[cfg(mobile)]
        let bottom = crate::phone::insets().1;
        #[cfg(not(mobile))]
        let bottom = 0.0;
        let area = egui::Area::new(Id::unique("toasts")).order(egui::Order::Tooltip).interactable(true);
        let area = if narrow {
            area.anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -(bottom + if self.core.active.is_some() { 104.0 } else { 32.0 })))
        } else {
            area.anchor(Align2::CENTER_TOP, Vec2::new(0.0, 104.0))
        };
        area.show(ctx, |ui| {
            ui.set_max_width(max_w);
            ui.vertical_centered(|ui| {
                for toast in &self.core.toasts {
                    let fill = match (toast.error, narrow) {
                        (true, _) => th::DANGER,
                        (false, true) => Color32::from_rgba_unmultiplied(40, 40, 40, 235),
                        (false, false) => th::TOAST,
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .corner_radius(if narrow { 22 } else { 8 })
                        .shadow(th::shadow_lg())
                        .inner_margin(if narrow { Margin::symmetric(18, 12) } else { Margin::symmetric(16, 10) })
                        .show(ui, |ui| {
                            ui.set_max_width(max_w - 36.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.add(egui::Label::new(text(&toast.text, if narrow { 14.0 } else { 13.5 }, W::Regular, Color32::WHITE)).wrap());
                                if let Some(p) = &toast.path {
                                    let link = ui.add(egui::Label::new(text(t("file.open"), 13.5, W::Semibold, Color32::WHITE).underline()).sense(Sense::click()));
                                    if link.clicked() {
                                        open = Some(p.clone());
                                    }
                                }
                            });
                        });
                    ui.add_space(8.0);
                }
            });
        });
        if let Some(p) = open {
            reveal(&p);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// .chat-col: head, log, form. Returns (send, close).
fn chat_column(ui: &mut Ui, icons: &Icons, chat: &mut Chat, with: &str) -> (bool, bool) {
    chat.unread = 0;
    let full = ui.max_rect();
    ui.painter().vline(full.left() + 0.5, full.y_range(), Stroke::new(1.0, th::BORDER));
    let mut close = false;
    // .chat-head
    let head = Rect::from_min_size(full.min, Vec2::new(full.width(), 48.0));
    ui.scope_builder(UiBuilder::new().max_rect(head.shrink2(Vec2::new(14.0, 8.0))), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(text(t("chat.title"), 14.0, W::Semibold, th::TEXT));
            let mut job = egui::text::LayoutJob::simple_singleline(with.to_string(), th::font(12.5, W::Regular), th::MUTED);
            job.wrap = egui::text::TextWrapping::truncate_at_width(full.width() - 150.0);
            ui.label(job);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if th::iconbtn(ui, icons, "x", &t("tt.close")).clicked() {
                    close = true;
                }
            });
        });
    });
    ui.painter().hline(full.x_range(), head.bottom(), Stroke::new(1.0, th::BORDER));
    // .chat-log, .chat-form at the bottom
    let form_h = 56.0;
    let log = Rect::from_min_max(Pos2::new(full.left() + 1.0, head.bottom() + 1.0), Pos2::new(full.right(), full.bottom() - form_h));
    ui.painter().rect_filled(log, 0.0, th::BG);
    ui.scope_builder(UiBuilder::new().max_rect(log.shrink(14.0)), |ui| {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
            if chat.lines.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| ui.label(text(t("chat.empty"), 12.5, W::Regular, th::MUTED)));
            }
            let maxw = (log.width() - 28.0) * 0.85;
            for l in &chat.lines {
                ui.with_layout(egui::Layout::top_down(if l.mine { egui::Align::Max } else { egui::Align::Min }), |ui| {
                    let (fill, stroke, fg) = if l.mine { (th::ACCENT, th::ACCENT, Color32::WHITE) } else { (th::SURFACE, th::BORDER, th::TEXT) };
                    let cr = if l.mine { CornerRadius { nw: 12, ne: 12, sw: 12, se: 4 } } else { CornerRadius { nw: 12, ne: 12, sw: 4, se: 12 } };
                    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(cr).inner_margin(Margin::symmetric(11, 7)).show(ui, |ui| {
                        ui.set_max_width(maxw);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(text(&l.text, 13.5, W::Regular, fg)).wrap());
                        ui.label(text(&l.time, 10.5, W::Regular, th::mix(fg, fill, 0.7)));
                    });
                });
                ui.add_space(8.0);
            }
        });
    });
    let form = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - form_h), full.max);
    ui.painter().hline(form.x_range(), form.top(), Stroke::new(1.0, th::BORDER));
    let mut send = false;
    ui.scope_builder(UiBuilder::new().max_rect(form.shrink(10.0)), |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let send_w = ui.painter().layout_no_wrap(t("chat.send"), th::font(13.5, W::Medium), th::TEXT).size().x + 28.0;
            let iw = ui.available_width() - send_w - 8.0;
            let id = Id::unique("chat-input").with(with);
            let focused = ui.ctx().memory(|m| m.has_focus(id));
            egui::Frame::new()
                .fill(if focused { th::SURFACE } else { th::SURFACE_2 })
                .stroke(Stroke::new(1.0, if focused { th::ACCENT } else { th::BORDER_STRONG }))
                .corner_radius(6)
                .inner_margin(Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut chat.input)
                            .id(id)
                            .frame(egui::Frame::NONE)
                            .hint_text(text(t("chat.ph"), 13.5, W::Regular, th::MUTED))
                            .char_limit(4000)
                            .desired_width(iw - 20.0)
                            .font(th::font(13.5, W::Regular)),
                    );
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        send = true;
                        r.request_focus();
                    }
                });
            if th::btn(ui, &t("chat.send"), Btn::Primary).clicked() {
                send = true;
            }
        });
    });
    (send, close)
}

/// The permission chips (.permtoggle) for a whole permission set; true when one changed.
fn perm_toggles(ui: &mut Ui, icons: &Icons, p: &mut crate::store::Permissions) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        for (icon, key, v) in [
            ("mouse", "perm.control", &mut p.control),
            ("clip", "perm.clipboard", &mut p.clipboard),
            ("file", "perm.files", &mut p.files),
            ("speaker", "perm.sound", &mut p.sound),
            ("camera", "perm.camera", &mut p.camera),
            ("power", "perm.restart", &mut p.restart),
            ("shield", "perm.privacy", &mut p.privacy),
            ("share", "perm.tunnel", &mut p.tunnel),
            ("terminal", "perm.terminal", &mut p.terminal),
        ] {
            changed |= th::permtoggle(ui, icons, icon, &t(key), v).changed();
        }
    });
    changed
}

/// The other side's camera window: its width in points, or folded to a round button.
#[derive(Clone, Copy)]
struct Pip {
    width: f32,
    folded: bool,
}

/// Where a `tex`-pixel picture goes in `stage` (points) so it fits whole. The host sends
/// the picture at about the stage's pixel size, give or take a pixel or two of rounding:
/// stretching it by those would resample every pixel and blur all the text, so near 1:1
/// it is drawn 1:1 — and always on whole pixels.
/// A dialog: centered on a desktop; on a phone near the top, so the keyboard that comes up
/// for its fields leaves them in sight (AnyDesk's dialogs move above the keyboard).
pub fn dialog_modal(ctx: &egui::Context, name: &str) -> egui::Modal {
    let id = Id::unique(name);
    let modal = egui::Modal::new(id);
    if !compact(ctx) {
        return modal;
    }
    #[cfg(mobile)]
    let top = crate::phone::insets().0 + 16.0;
    #[cfg(not(mobile))]
    let top = 16.0;
    modal.area(egui::Modal::default_area(id.with("area")).anchor(Align2::CENTER_TOP, Vec2::new(0.0, top)))
}

/// A dialog's width: as asked, but inside a phone's screen.
pub fn dialog_width(ctx: &egui::Context, want: f32) -> f32 {
    want.min(ctx.content_rect().width() - 32.0 - 44.0)
}

/// The chat column: beside the session on a desktop, the whole width on a phone.
fn chat_width(ctx: &egui::Context) -> f32 {
    if compact(ctx) {
        ctx.content_rect().width()
    } else {
        CHAT_W
    }
}

fn fit_rect(stage: Rect, tex: Vec2, ppp: f32) -> Rect {
    let stage_px = stage.size() * ppp;
    let mut scale = (stage_px.x / tex.x).min(stage_px.y / tex.y);
    if (0.97..=1.03).contains(&scale) {
        scale = 1.0;
    }
    let size_px = (tex * scale).round();
    let min_px = (stage.center().to_vec2() * ppp - size_px / 2.0).round();
    Rect::from_min_size((min_px / ppp).to_pos2(), size_px / ppp)
}

/// Translated keyboard: does this key just type a character (sent as text instead)?
fn types_text(code: &str, m: egui::Modifiers, typed: bool) -> bool {
    if code.starts_with("Shift") {
        return true;
    }
    let printable = code.starts_with("Key")
        || code.starts_with("Digit")
        || matches!(code, "Space" | "Minus" | "Equal" | "BracketLeft" | "BracketRight" | "Backslash" | "Semicolon" | "Quote" | "Comma" | "Period" | "Slash" | "Backquote" | "IntlBackslash");
    // Ctrl/Cmd make a shortcut; Alt does too unless it typed something (AltGr, Option).
    printable && !m.command && !m.ctrl && !m.mac_cmd && (!m.alt || typed)
}

fn is_switch_keys(k: egui::Key, m: egui::Modifiers) -> bool {
    m.ctrl && m.alt && m.shift && matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight)
}

/// The address field after an edit: digits only (at most 9), grouped "123 456 789", and
/// where the cursor goes — after the same digit it followed in the edited text.
fn reformat_id(edited: &str, cursor: usize) -> (String, usize) {
    let digits_before = edited.chars().take(cursor).filter(|c| c.is_ascii_digit()).count();
    let d: String = edited.chars().filter(|c| c.is_ascii_digit()).take(9).collect();
    let text = rv::format_id(&d);
    let n = digits_before.min(d.len());
    let at = if n == 0 { 0 } else { text.char_indices().filter(|(_, c)| c.is_ascii_digit()).nth(n - 1).map(|(i, _)| i + 1).unwrap_or(text.len()) };
    (text, at)
}

/// A computer the address bar suggests.
struct Suggestion {
    id: String,
    name: String,
    ip: String,
    lan: bool,
}

/// Letters in the address bar (and not a whole ID pasted with them): a name is looked up.
fn is_name_query(q: &str) -> bool {
    q.chars().any(|c| c.is_alphabetic()) && q.chars().filter(|c| c.is_ascii_digit()).count() < 9
}

/// Highlighted at first: the best match, unless what is typed is already a whole ID or an IP.
fn default_pick(q: &str) -> Option<usize> {
    let digits = q.chars().filter(|c| c.is_ascii_digit()).count();
    let whole = (digits == 9 && !is_name_query(q)) || q.contains(['.', ':']);
    (!q.trim().is_empty() && !whole).then_some(0)
}

/// Recent and LAN computers matching the address bar: by digits of the ID, by name, or by
/// the start of the LAN address (AnyDesk's autocomplete). At most six.
fn suggestions(core: &crate::core::Core, q: &str) -> Vec<Suggestion> {
    let q = q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
    let low = q.to_lowercase();
    let name_q = is_name_query(q);
    let hit = |id: &str, name: &str, ip: &str| {
        if name_q {
            name.to_lowercase().contains(&low)
        } else if q.contains('.') {
            ip.starts_with(q)
        } else {
            !digits.is_empty() && id.contains(&digits)
        }
    };
    let lan = crate::lan::peers();
    let mut out: Vec<Suggestion> = Vec::new();
    for r in &core.store.recent {
        let peer = lan.iter().find(|p| p.id == r.id);
        let ip = peer.map(|p| p.ip.to_string()).unwrap_or_default();
        if hit(&r.id, &r.name, &ip) {
            out.push(Suggestion { id: r.id.clone(), name: r.name.clone(), ip, lan: peer.is_some() });
        }
    }
    for p in &lan {
        let ip = p.ip.to_string();
        if !out.iter().any(|s| s.id == p.id) && hit(&p.id, &p.name, &ip) {
            out.push(Suggestion { id: p.id.clone(), name: p.name.clone(), ip, lan: true });
        }
    }
    out.truncate(6);
    out
}

/// The session's tag in the toolbar: what it can do (a desktop) or what it is.
fn mode_tag(mode: crate::core::SessMode, control: bool) -> String {
    match mode {
        crate::core::SessMode::Files => t("mode.files"),
        crate::core::SessMode::Terminal => t("mode.terminal"),
        crate::core::SessMode::Desktop => t(if control { "perm.ctlOn" } else { "perm.viewOnly" }),
    }
}

/// The icon of a session of this mode (tabs, requests).
fn mode_icon(mode: crate::core::SessMode) -> &'static str {
    match mode {
        crate::core::SessMode::Files => "file",
        crate::core::SessMode::Terminal => "terminal",
        crate::core::SessMode::Desktop => "monitor",
    }
}

/// A QR code (black modules on white, with a quiet zone).
fn qr(ui: &mut Ui, data: &str, size: f32) {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else { return };
    let n = code.width();
    let colors = code.to_colors();
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(r, 6.0, Color32::WHITE);
    let cell = (size - 16.0) / n as f32;
    let o = r.min + Vec2::splat(8.0);
    for (k, c) in colors.iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = ((k % n) as f32, (k / n) as f32);
            ui.painter().rect_filled(Rect::from_min_size(o + Vec2::new(x * cell, y * cell), Vec2::splat(cell + 0.3)), 0.0, Color32::BLACK);
        }
    }
}

/// .field: a small label above an input box.
fn field(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(text(label, 12.5, W::Regular, th::MUTED));
    ui.add_space(4.0);
    egui::Frame::new().fill(th::SURFACE_2).stroke(Stroke::new(1.0, th::BORDER_STRONG)).corner_radius(6).inner_margin(Margin::symmetric(10, 7)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(10.0);
}

fn input(ui: &mut Ui, value: &mut String, password: bool) {
    ui.add(egui::TextEdit::singleline(value).frame(egui::Frame::NONE).password(password).font(th::font(13.5, W::Regular)).desired_width(f32::INFINITY));
}

/// A small rounded tag (.sas.small, .perm-tag): 20 px tall whatever the row height.
fn pill(ui: &mut Ui, font: egui::FontId, spacing: f32, s: &str, fg: Color32, bg: Color32) -> egui::Response {
    let mut job = egui::text::LayoutJob::default();
    job.append(s, 0.0, egui::TextFormat { font_id: font, color: fg, extra_letter_spacing: spacing, ..Default::default() });
    let g = ui.painter().layout_job(job);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 16.0, 22.0), Sense::hover());
    ui.painter().rect_filled(r, 4.0, bg);
    ui.painter().galley(r.center() - g.size() / 2.0, g, fg);
    resp
}

fn time_ago(ts: i64) -> String {
    let m = ((rv::now_ms() - ts) as f64 / 60000.0).round() as i64;
    if m < 1 {
        t("time.now")
    } else if m < 60 {
        tf("time.min", &[("n", &m.to_string())])
    } else if m < 1440 {
        tf("time.hour", &[("n", &((m as f64 / 60.0).round() as i64).to_string())])
    } else {
        chrono::DateTime::from_timestamp_millis(ts).map(|d| d.with_timezone(&chrono::Local).format("%d.%m.%Y").to_string()).unwrap_or_default()
    }
}

/// The touchpad pointer: an arrow with its tip at `p`.
fn paint_arrow(painter: &egui::Painter, p: Pos2) {
    let at = |pts: &[(f32, f32)]| pts.iter().map(|&(x, y)| p + Vec2::new(x, y)).collect::<Vec<_>>();
    // the tail and the head (each convex), then the outline around both
    painter.add(egui::Shape::convex_polygon(at(&[(4.6, 12.0), (7.0, 11.4), (10.2, 18.4), (7.6, 19.6)]), Color32::WHITE, Stroke::NONE));
    painter.add(egui::Shape::convex_polygon(at(&[(0.0, 0.0), (12.5, 12.2), (0.0, 17.2)]), Color32::WHITE, Stroke::NONE));
    let outline = at(&[(0.0, 0.0), (12.5, 12.2), (7.0, 12.2), (10.2, 18.4), (7.6, 19.6), (4.6, 13.0), (0.0, 17.2), (0.0, 0.0)]);
    painter.add(egui::Shape::line(outline, Stroke::new(1.2, Color32::BLACK)));
}

struct Touch {
    start: Pos2,
    last: Pos2,
    at: Instant,
    /// moved past the slop (touch mode: the mouse button is down on the host)
    dragging: bool,
    /// taken by a long press or a second finger
    done: bool,
    /// touchpad: pressed right after a tap — the move drags (button held)
    hold: bool,
    /// touchpad: the held button went down on the host
    down_sent: bool,
}

/// Touchpad: how far the pointer goes for a finger's travel (the picture is small).
const TP_SPEED: f32 = 1.6;

/// A phone-sized screen (or window): pages stack instead of sitting beside the sidebar.
pub fn compact(ctx: &egui::Context) -> bool {
    cfg!(mobile) || ctx.content_rect().width() < 640.0
}

fn thumbnail(pic: &crate::video::Picture) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(pic.width as u32, pic.height as u32, pic.rgba.clone())?;
    let small = image::imageops::thumbnail(&img, 480, (480 * pic.height / pic.width.max(1)).max(1) as u32);
    let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    rgb.write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
    Some(out.into_inner())
}

fn reveal(p: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(p)).spawn();
    #[cfg(mobile)]
    let _ = p;
}

#[cfg(test)]
mod tests {
    use super::{default_pick, is_name_query, reformat_id};

    #[test]
    fn address_queries() {
        // letters: a name is looked up (a pasted "ID: 123 456 789" is still an ID)
        assert!(is_name_query("shuk"));
        assert!(!is_name_query("677 725"));
        assert!(!is_name_query("ID: 677 725 472"));
        // the best match is highlighted, unless a whole ID or an IP is typed
        assert_eq!(default_pick("67"), Some(0));
        assert_eq!(default_pick("Mac"), Some(0));
        assert_eq!(default_pick("677 725 472"), None);
        assert_eq!(default_pick("192.168.1.20"), None);
        assert_eq!(default_pick("  "), None);
    }

    /// Types `keys` one by one at the cursor, like the field does.
    fn type_keys(keys: &str) -> (String, usize) {
        let (mut text, mut at) = (String::new(), 0usize);
        for k in keys.chars() {
            let mut chars: Vec<char> = text.chars().collect();
            chars.insert(at, k);
            (text, at) = reformat_id(&chars.into_iter().collect::<String>(), at + 1);
        }
        (text, at)
    }

    #[test]
    fn fit_is_pixel_exact_near_one_to_one() {
        use egui::{Pos2, Rect, Vec2};
        // 733×777 pt window case: a 1464×952 picture into a 1466 px wide stage
        let stage = Rect::from_min_size(Pos2::new(0.0, 96.0), Vec2::new(733.0, 681.0));
        let r = super::fit_rect(stage, Vec2::new(1464.0, 952.0), 2.0);
        assert_eq!(r.size() * 2.0, Vec2::new(1464.0, 952.0));
        assert_eq!((r.min.x * 2.0).fract(), 0.0);
        assert_eq!((r.min.y * 2.0).fract(), 0.0);
        // far from 1:1 (a small window): scaled to fit, still on whole pixels
        let small = Rect::from_min_size(Pos2::new(0.0, 96.0), Vec2::new(300.0, 200.0));
        let r = super::fit_rect(small, Vec2::new(3024.0, 1964.0), 2.0);
        assert!(r.width() <= 300.0 + 0.5 && r.height() <= 200.0 + 0.5);
        assert_eq!((r.min.x * 2.0).fract(), 0.0);
    }

    #[test]
    fn typing_keeps_order() {
        assert_eq!(type_keys("123456789"), ("123 456 789".into(), 11));
        assert_eq!(type_keys("12345678901"), ("123 456 789".into(), 11)); // extra digits dropped
        assert_eq!(type_keys("12a3"), ("123".into(), 3));
    }

    #[test]
    fn paste_with_noise() {
        assert_eq!(reformat_id("ID: 123-456-789\n", 16), ("123 456 789".into(), 11));
        assert_eq!(reformat_id("123456789", 9), ("123 456 789".into(), 11));
    }

    #[test]
    fn edit_in_the_middle() {
        // "123 456" with the cursor after "1": typing 9 → "192 345 6", cursor after the 9
        assert_eq!(reformat_id("1923 456", 2), ("192 345 6".into(), 2));
        // backspace over the space after "123" removes nothing visible but keeps the cursor
        assert_eq!(reformat_id("123456", 3), ("123 456".into(), 3));
    }
}

/// Android drops the window while ManyDesk is in the background, and eframe then runs
/// nothing of the app — no core ticks: requests went unanswered and sessions stalled
/// until the user came back. A thread ticks the core instead whenever the UI thread has
/// been quiet for a while; the two take turns through `TURN` (the UI thread holds it for
/// each logic and ui pass), so the core never runs on both at once.
#[cfg(target_os = "android")]
mod background {
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::{Mutex, MutexGuard, Once};
    use std::time::Duration;

    use super::App;

    static TURN: Mutex<()> = Mutex::new(());
    /// The app, boxed by eframe: its address holds for the process.
    static APP: AtomicPtr<App> = AtomicPtr::new(std::ptr::null_mut());

    struct Ptr(*mut App);
    // Only dereferenced while holding TURN, i.e. while the UI thread is not in the app.
    unsafe impl Send for Ptr {}

    /// The UI thread's turn; also registers the app and starts the background tick.
    pub fn claim(app: &mut App) -> MutexGuard<'static, ()> {
        APP.store(app as *mut App, Ordering::Release);
        static START: Once = Once::new();
        START.call_once(|| {
            std::thread::Builder::new()
                .name("background-tick".into())
                .spawn(run)
                .ok();
        });
        TURN.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run() {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if crate::perf::since_beat_ms() < 300 {
                continue; // the UI thread is ticking
            }
            let Ok(_turn) = TURN.try_lock() else { continue };
            let p = Ptr(APP.load(Ordering::Acquire));
            if !p.0.is_null() {
                unsafe { (*p.0).tick_core() };
            }
        }
    }
}
