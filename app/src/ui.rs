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
    fullscreen: bool,
    tb_hide_at: Option<Instant>,
    thumbs: HashMap<String, (usize, TextureHandle, TextureHandle)>,
    screen_thumbs: HashMap<u32, TextureHandle>,
    req_perms: crate::store::Permissions,
    frames: HashMap<u32, usize>,
    mac_local: bool,
    host_chat: Option<u32>,
    collapsed: HashSet<u32>,
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
    update_started: bool,
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
/// the newest update's download, after a successful check
static UPDATE_URL: Mutex<Option<String>> = Mutex::new(None);

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
        th::setup(&cc.egui_ctx);
        let mut core = Core::new();
        // Test hook: MANYDESK_AUTOCONNECT=<id> connects on start (headless tests).
        if let Ok(id) = std::env::var("MANYDESK_AUTOCONNECT") {
            core.connect(&id);
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
        App {
            req_perms: core.store.perms,
            core,
            rx,
            icons: Icons::default(),
            page,
            address: String::new(),
            fullscreen: false,
            tb_hide_at: None,
            thumbs: HashMap::new(),
            screen_thumbs: HashMap::new(),
            frames: HashMap::new(),
            mac_local: cfg!(target_os = "macos"),
            host_chat: None,
            collapsed: HashSet::new(),
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
            update_started: false,
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

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        crate::perf::beat();
        crate::perf::phase("logic");
        // Test hook: MANYDESK_MINIMIZE=1 minimizes the window once it is up (hidden host).
        if ctx.cumulative_pass_nr() == 3 && std::env::var_os("MANYDESK_MINIMIZE").is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        crate::perf::phase("core.tick");
        self.core.tick(&self.rx);
        crate::perf::phase("logic");
        // Test hook: accept requests even while the window is hidden (no ui pass then).
        if self.core.pending.is_some() && std::env::var_os("MANYDESK_AUTOACCEPT").is_some() {
            // tests may ask for every permission (restart, privacy, tunnel included)
            let perms = if std::env::var_os("MANYDESK_AUTOFULL").is_some() { crate::store::Permissions::FULL } else { self.req_perms };
            self.core.decide(true, perms);
        }
        self.core.apply_isolation();
        // Keep ticking while sessions exist, also when the window is hidden (then eframe
        // runs only this, not ui()).
        if !self.core.tabs.is_empty() || !self.core.hosts.is_empty() || self.core.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        crate::perf::phase("os");
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        crate::perf::tick("uiframe");
        crate::perf::phase("ui");
        if self.minimize_once {
            self.minimize_once = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            if self.core.store.autoupdate {
                self.check_update(false);
            }
        }
        if !self.update_started {
            self.update_started = true;
            if self.core.store.autoupdate && !std::env::args().any(|a| a == "--background") {
                self.check_update(false);
            }
        }
        let active = self.core.active.filter(|k| self.core.tabs.iter().any(|t| t.key == *k));
        if !(self.fullscreen && active.is_some()) {
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
        self.pointer_overlay(&ctx);
        self.privacy_cover(&ctx);
        self.act_dialog(&ctx);
        self.toasts(&ctx);
        crate::perf::phase("os");
    }
}

impl App {
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
        self.icons.paint(ui, if tab.is_some() { "monitor" } else { "home" }, Rect::from_center_size(Pos2::new(cx + 9.0, cy), Vec2::splat(18.0)), fg);
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
        let input = Rect::from_min_max(Pos2::new(ab.left() + 10.0 + 18.0 + 8.0, ab.top() + 4.0), Pos2::new(go.left() - 8.0, ab.bottom() - 4.0));
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
        let gr = ui.interact(go, Id::unique("rv-connect"), Sense::click()).on_hover_text(t("tt.connect"));
        ui.painter().rect_filled(go, 4.0, if gr.hovered() { th::ACCENT_HOVER } else { th::ACCENT });
        self.icons.paint(ui, "arrow", Rect::from_center_size(go.center(), Vec2::splat(18.0)), Color32::WHITE);
        if gr.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if (gr.clicked() || submit) && !self.address.trim().is_empty() {
            let a = std::mem::take(&mut self.address);
            self.core.connect(&a);
        }
    }

    // ================================================================ home ===

    fn home(&mut self, ui: &mut Ui) {
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
        let gap = 16.0;
        let cols = (((width + gap) / (210.0 + gap)).floor() as usize).max(1);
        let tw = (width - gap * (cols as f32 - 1.0)) / cols as f32;
        let th_h = tw * 10.0 / 16.0;
        let tile_h = th_h + 58.0;
        let rows = recents.len().div_ceil(cols);
        let (area, _) = ui.allocate_exact_size(Vec2::new(width, rows as f32 * (tile_h + gap) - gap), Sense::hover());
        let mut connect = None;
        let mut forget = None;
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
            let tg = p.layout_no_wrap(time_ago(r.ts), th::font(11.0, W::Regular), th::MUTED);
            p.galley(Pos2::new(meta.right() - tg.size().x, meta.top() + 19.0 - tg.size().y / 2.0), tg.clone(), th::MUTED);
            let name_w = meta.width() - tg.size().x - 8.0;
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
            if hover {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() && !removed {
                connect = Some(r.id.clone());
            }
        }
        if let Some(id) = forget {
            self.core.store.forget(&id);
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
        self.page_title(ui, "nav.share");
        if cfg!(target_os = "macos") {
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
                        self.core.store.save();
                    }
                });
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

    /// Checks for a newer ManyDesk; `manual` shows "up to date" too.
    fn check_update(&mut self, manual: bool) {
        *self.update.lock().unwrap() = Some((t("start.checking"), None));
        let slot = self.update.clone();
        let cur = env!("CARGO_PKG_VERSION").to_string();
        crate::rt::spawn(async move {
            let msg = match crate::platform::update::check(&cur).await {
                Ok(Some(u)) => {
                    *slot.lock().unwrap() = Some((tf("start.found", &[("v", &u.version)]), Some(true)));
                    bus::wake();
                    *UPDATE_URL.lock().unwrap() = Some(u.url);
                    return;
                }
                Ok(None) if manual => (t("start.upToDate"), Some(false)),
                Ok(None) => return,
                Err(e) if manual => (tf("start.checkFailed", &[("e", &e)]), Some(false)),
                Err(_) => return,
            };
            *slot.lock().unwrap() = Some(msg);
            bus::wake();
        });
    }

    fn do_update(&mut self) {
        let Some(url) = UPDATE_URL.lock().unwrap().clone() else { return };
        self.core.toast(t("start.downloading"), false);
        crate::rt::spawn(async move {
            if let Err(e) = crate::platform::update::download_and_open(&url).await {
                bus::emit("toast", json!({ "text": e, "error": true }));
            }
        });
    }

    fn page_settings(&mut self, ui: &mut Ui) {
        self.page_title(ui, "nav.settings");
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
        self.startup_panel(ui);
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
        let fs = self.fullscreen;
        if !fs {
            egui::Panel::top("toolbar").exact_size(TOOLBAR_H).frame(egui::Frame::new().fill(th::SURFACE).inner_margin(Margin::symmetric(10, 0))).show(ui, |ui| {
                let r = ui.max_rect().expand2(Vec2::new(10.0, 0.0));
                ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, th::BORDER));
                self.toolbar(ui, key);
            });
        }
        if self.core.tabs[i].chat.open {
            let label = self.core.tabs[i].label.clone();
            egui::Panel::right("chat").exact_size(CHAT_W).resizable(false).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
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
            if tab.state == TabState::Live && tab.fm.open {
                self.file_manager(ui, stage, key);
                self.fs_toolbar(&ctx, stage, key);
                return;
            }
            let tab = &self.core.tabs[i];
            if tab.state != TabState::Live || tab.texture.is_none() {
                self.connecting_card(ui, stage, i, key);
                return;
            }
            let tex = tab.texture.clone().unwrap();
            let control = tab.remote.control;
            let capturing = self.core.capture_tab == Some(key);
            let resp = ui.allocate_rect(stage, Sense::click_and_drag());
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
            let painter = ui.painter().with_clip_rect(stage);
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
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
            if !capturing && control && !self.boards.get(&key).is_some_and(|b| b.on) {
                let g = ui.painter().layout_no_wrap(t("sess.click"), th::font(12.5, W::Regular), th::TEXT);
                let hint = Rect::from_center_size(Pos2::new(stage.center().x, stage.bottom() - 20.0 - 16.0), Vec2::new(g.size().x + 28.0, 32.0));
                ui.painter().add(th::shadow().as_shape(hint, CornerRadius::same(16)));
                ui.painter().rect_filled(hint, 16.0, Color32::from_white_alpha(242));
                ui.painter().galley(hint.center() - g.size() / 2.0, g, th::TEXT);
            }
            if self.board(ui, &resp, key, rect, stage) {
                self.fs_toolbar(&ctx, stage, key);
                return;
            }
            if resp.clicked() && !capturing && control {
                self.core.set_capture(Some(key));
                return;
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
                    if a.totp {
                        field(ui, &t("auth.code"), |ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut a.code).char_limit(6).frame(egui::Frame::NONE).font(th::mono(15.0, W::Semibold)).desired_width(f32::INFINITY));
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                submit = true;
                            }
                        });
                    }
                    th::checkbox(ui, &mut a.remember, &t("auth.remember"));
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
        let (screens, cur_screen, view) = (tab.screens.clone(), tab.screen, tab.view);
        let (fm_open, fm_busy) = (tab.fm.open, tab.fm.transfers.iter().filter(|t| !t.finished).count());
        let (privacy_on, board_on) = (tab.privacy, self.boards.get(&key).is_some_and(|b| b.on));
        ui.horizontal_centered(|ui| {
            // .sess-name
            ui.spacing_mut().item_spacing.x = 8.0;
            if live {
                th::pulse(ui);
            } else {
                th::dot(ui, th::WARN);
            }
            ui.label(text(&label, 13.5, W::Semibold, th::TEXT));
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
            if th::tbbtn(ui, &self.icons, "mic", mic_on, false, &t(if mic_on { "tt.micOn" } else { "tt.mic" })).clicked() && live {
                self.core.set_mic(Some(key), None, !mic_on);
            }
            let r = th::tbbtn(ui, &self.icons, "file", fm_open, false, &t("tt.fm"));
            th::badge(ui, r.rect, fm_busy);
            if r.clicked() && live {
                self.core.fm_toggle(key);
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
                    let tag = t(if control { "perm.ctlOn" } else { "perm.viewOnly" });
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
                    let (tag, fg, bg) = if control { (t("perm.ctlOn"), th::OK, th::OK_SOFT) } else { (t("perm.viewOnly"), th::MUTED, th::SURFACE_2) };
                    pill(ui, th::font(11.5, W::Medium), 0.0, &format!("{os} · {tag}"), fg, bg);
                }
            });
        });
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
        for ev in events {
            match ev {
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
                egui::Event::Text(s) if text_mode => out.push(json!({ "t": "text", "s": s })),
                egui::Event::Key { key: k, physical_key, modifiers, .. } if text_mode && keys::code(physical_key.unwrap_or(k)).is_some_and(|c| types_text(c, modifiers, typed)) => {}
                egui::Event::Key { key: k, physical_key, pressed, .. } => {
                    if let Some(code) = keys::code(physical_key.unwrap_or(k)) {
                        let code = if swap { keys::swap_mods(code) } else { code };
                        out.push(json!({ "t": "key", "code": code, "d": pressed }));
                    }
                }
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => {
                    let code = match ev {
                        egui::Event::Copy => "KeyC",
                        egui::Event::Cut => "KeyX",
                        _ => "KeyV",
                    };
                    out.push(json!({ "t": "key", "code": code, "d": true }));
                    out.push(json!({ "t": "key", "code": code, "d": false }));
                }
                egui::Event::WindowFocused(false) => self.core.set_capture(None),
                _ => {}
            }
        }
        if let Some((x, y)) = moved {
            self.core.input(key, json!({ "t": "move", "x": x, "y": y }));
        }
        for m in out {
            self.core.input(key, m);
        }
    }

    // ============================================================= file manager ===

    /// AnyDesk-style file transfer: this computer on the left, the host on the right, the
    /// copies in progress underneath. Takes the place of the remote screen while open.
    fn file_manager(&mut self, ui: &mut Ui, stage: Rect, key: u32) {
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
        let modal = egui::Modal::new(Id::unique("fm-dialog")).frame(egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).shadow(th::shadow_lg()).inner_margin(Margin::same(22))).show(ctx, |ui| {
            ui.set_width(340.0);
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
        let left = 60u64.saturating_sub(req.at.elapsed().as_secs());
        let mut decision = None;
        let w = 400.0_f32.min(screen.width() - 32.0);
        egui::Area::new(Id::unique("accept")).order(egui::Order::Tooltip).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).shadow(th::shadow_lg()).inner_margin(22).show(ui, |ui| {
                ui.set_width(w - 44.0);
                self.accept_head(ui, &req.name, &req.os, &req.from, None);
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
            return;
        }
        let right = 18.0 + if self.host_chat.is_some() { CHAT_W } else { 0.0 };
        let mut actions: Vec<(u32, &'static str, bool, bool)> = Vec::new();
        let mut perm_change = None;
        let hosts: Vec<u32> = self.core.hosts.iter().map(|h| h.key).collect();
        egui::Area::new(Id::unique("host-stack")).anchor(Align2::RIGHT_BOTTOM, Vec2::new(-right, -18.0)).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_width(360.0);
            for key in hosts {
                let Some(h) = self.core.hosts.iter().find(|h| h.key == key) else { continue };
                let color = Color32::from_rgb(h.color[0], h.color[1], h.color[2]);
                let live = h.state == HostState::Live;
                let collapsed = self.collapsed.contains(&key);
                let chat_active = self.host_chat == Some(key);
                let (name, from, sas, stats, remote_mic, unread, mic_on) = (h.req.name.clone(), h.req.from.clone(), h.sas.clone(), h.stats.clone(), h.remote_mic, h.chat.unread, h.mic.is_some());
                let mut perms = h.perms;
                let waiting = h.auth.is_some();
                let fr = egui::Frame::new()
                    .fill(th::SURFACE)
                    .stroke(Stroke::new(1.0, if chat_active { th::ACCENT } else { th::BORDER }))
                    .corner_radius(12)
                    .shadow(th::shadow_lg())
                    .inner_margin(Margin { left: 20, right: 16, top: 14, bottom: 14 })
                    .show(ui, |ui| {
                        ui.set_width(360.0 - 36.0);
                        ui.horizontal(|ui| {
                            let state = if waiting { t("host.authWait") } else if live { t("host.connected") } else { t("host.connecting") };
                            ui.scope(|ui| {
                                ui.set_width(360.0 - 36.0 - 40.0);
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
                "file" => {
                    let sid = self.core.hosts.iter().find(|h| h.key == k).and_then(|h| h.sid);
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        self.core.send_files(sid, files);
                    }
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
        egui::Panel::right("host-chat").exact_size(CHAT_W).resizable(false).frame(egui::Frame::new().fill(th::SURFACE)).show(ui, |ui| {
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
        egui::Area::new(Id::unique("toasts")).anchor(Align2::CENTER_TOP, Vec2::new(0.0, 104.0)).order(egui::Order::Tooltip).interactable(true).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                for toast in &self.core.toasts {
                    egui::Frame::new()
                        .fill(if toast.error { th::DANGER } else { th::TOAST })
                        .corner_radius(8)
                        .shadow(th::shadow_lg())
                        .inner_margin(Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(520.0 - 32.0);
                            ui.horizontal(|ui| {
                                ui.label(text(&toast.text, 13.5, W::Regular, Color32::WHITE));
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
            ("power", "perm.restart", &mut p.restart),
            ("shield", "perm.privacy", &mut p.privacy),
            ("share", "perm.tunnel", &mut p.tunnel),
        ] {
            changed |= th::permtoggle(ui, icons, icon, &t(key), v).changed();
        }
    });
    changed
}

/// Where a `tex`-pixel picture goes in `stage` (points) so it fits whole. The host sends
/// the picture at about the stage's pixel size, give or take a pixel or two of rounding:
/// stretching it by those would resample every pixel and blur all the text, so near 1:1
/// it is drawn 1:1 — and always on whole pixels.
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
}

#[cfg(test)]
mod tests {
    use super::reformat_id;

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
