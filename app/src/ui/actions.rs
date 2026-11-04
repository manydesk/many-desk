// The session's Actions menu (special keys, keyboard mode, privacy, lock on end,
// screenshot, whiteboard, switch sides, restart, TCP tunnels) and the host-side covers
// it brings: the privacy screen and the viewer's drawings.
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Id, Margin, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::*;

/// Viewer: the whiteboard of one session (strokes normalized to the remote screen).
#[derive(Default)]
pub struct Board {
    pub on: bool,
    pub color: usize,
    pub strokes: Vec<(u64, [u8; 3], Vec<(f32, f32)>)>,
    /// the stroke being drawn, and the points not sent yet
    cur: Option<u64>,
    unsent: Vec<(f32, f32)>,
    last_send: Option<Instant>,
}

pub const BOARD_COLORS: [[u8; 3]; 5] = [[229, 70, 59], [31, 157, 85], [37, 99, 235], [217, 139, 11], [17, 17, 17]];

pub enum ActDialog {
    Restart { key: u32 },
    Tunnel { key: u32, local: String, host: String, port: String, error: String },
}

impl App {
    /// The ⋯ button's menu. Called inside the toolbar row.
    pub(super) fn actions_menu(&mut self, ui: &mut Ui, key: u32) {
        let Some(tab) = self.core.tabs.iter().find(|t| t.key == key) else { return };
        let (os, perms, privacy, lock_end, kbd_text, live) = (tab.os.clone(), tab.remote, tab.privacy, tab.lock_on_end, tab.kbd_text, tab.state == TabState::Live);
        let board_on = self.boards.get(&key).is_some_and(|b| b.on);
        let has_strokes = self.boards.get(&key).is_some_and(|b| !b.strokes.is_empty());
        let (mac, win) = (os == "darwin", os == "win32");
        let section = |ui: &mut Ui, s: String| {
            ui.add_space(2.0);
            ui.label(text(s, 11.5, W::Medium, th::MUTED));
        };
        ui.set_min_width(250.0);
        // special keys
        section(ui, t("act.keys"));
        ui.add_enabled_ui(live && perms.control, |ui| {
            if ui.button(if mac { t("act.forceQuit") } else { "Ctrl+Alt+Del".into() }).clicked() {
                if win {
                    self.core.special(key, "cad");
                } else if mac {
                    self.core.send_combo(key, &["MetaLeft", "AltLeft", "Escape"]);
                } else {
                    self.core.send_combo(key, &["ControlLeft", "AltLeft", "Delete"]);
                }
            }
            if ui.button(t("act.lock")).clicked() {
                self.core.special(key, "lock");
            }
            if ui.button(if mac { "Cmd+Tab" } else { "Alt+Tab" }).clicked() {
                self.core.send_combo(key, &[if mac { "MetaLeft" } else { "AltLeft" }, "Tab"]);
            }
            if ui.button(if mac { "Cmd+Space" } else if win { "Win" } else { "Super" }).clicked() {
                if mac {
                    self.core.send_combo(key, &["MetaLeft", "Space"]);
                } else {
                    self.core.send_combo(key, &["MetaLeft"]);
                }
            }
            if ui.button(if mac { "Cmd+Shift+3" } else { "PrintScreen" }).clicked() {
                if mac {
                    self.core.send_combo(key, &["MetaLeft", "ShiftLeft", "Digit3"]);
                } else {
                    self.core.send_combo(key, &["PrintScreen"]);
                }
            }
            if win && ui.button("Ctrl+Shift+Esc").clicked() {
                self.core.send_combo(key, &["ControlLeft", "ShiftLeft", "Escape"]);
            }
        });
        ui.separator();
        // keyboard mode
        section(ui, t("kbd.title"));
        let mut text_mode = kbd_text;
        ui.radio_value(&mut text_mode, false, t("kbd.keys")).on_hover_text(t("kbd.keysHint"));
        ui.radio_value(&mut text_mode, true, t("kbd.text")).on_hover_text(t("kbd.textHint"));
        if text_mode != kbd_text {
            if let Some(tab) = self.core.tab(key) {
                tab.kbd_text = text_mode;
            }
        }
        ui.separator();
        // session switches
        let mut p = privacy;
        if ui.add_enabled(live && perms.privacy, egui::Checkbox::new(&mut p, t("perm.privacy"))).on_disabled_hover_text(t("act.needPerm")).changed() {
            self.core.set_privacy(key, p);
        }
        let mut l = lock_end;
        if ui.add_enabled(live && perms.control, egui::Checkbox::new(&mut l, t("act.lockEnd"))).changed() {
            self.core.set_lock_on_end(key, l);
        }
        ui.separator();
        // the remote display's resolution (asked for when the menu opens)
        section(ui, t("res.title"));
        if live && perms.control {
            self.core.want_modes(key);
            let modes = self.core.tabs.iter().find(|t| t.key == key).and_then(|t| t.modes.clone()).unwrap_or_default();
            if modes.virtual_ok && !modes.asked {
                let mut vd = modes.vd_on;
                if ui.checkbox(&mut vd, t("vd.add")).on_hover_text(t("vd.hint")).changed() {
                    self.core.set_vdisplay(key, vd);
                    ui.close();
                }
            }
            if modes.asked {
                ui.add(egui::Spinner::new().size(14.0).color(th::MUTED));
            } else if modes.list.is_empty() {
                ui.label(text(t("res.none"), 12.5, W::Regular, th::MUTED));
            } else {
                egui::ScrollArea::vertical().id_salt(("res", key)).max_height(170.0).show(ui, |ui| {
                    for m in &modes.list {
                        let cur = modes.cur.is_some_and(|c| c.w == m.w && c.h == m.h);
                        let label = if m.hz > 0 { format!("{} × {}  ·  {} Hz", m.w, m.h, m.hz) } else { format!("{} × {}", m.w, m.h) };
                        if ui.selectable_label(cur, label).clicked() && !cur {
                            self.core.set_mode(key, Some(*m));
                            ui.close();
                        }
                    }
                });
                if modes.changed && ui.button(t("res.orig")).clicked() {
                    self.core.set_mode(key, None);
                    ui.close();
                }
            }
        } else {
            ui.label(text(t("act.needPerm"), 12.5, W::Regular, th::MUTED));
        }
        ui.separator();
        if ui.add_enabled(live && self.last_pic.contains_key(&key), egui::Button::new(t("act.screenshot"))).clicked() {
            self.screenshot(key);
        }
        let mut b = board_on;
        if ui.add_enabled(live && perms.control, egui::Checkbox::new(&mut b, t("act.board"))).changed() {
            let board = self.boards.entry(key).or_default();
            board.on = b;
            if b {
                self.core.set_capture(None);
            }
        }
        if has_strokes && ui.button(t("act.boardClear")).clicked() {
            if let Some(board) = self.boards.get_mut(&key) {
                board.strokes.clear();
            }
            self.core.draw_clear(key);
        }
        ui.separator();
        if ui.add_enabled(live && perms.tunnel, egui::Button::new(t("act.tunnel"))).on_disabled_hover_text(t("act.needPerm")).clicked() {
            self.act_dialog = Some(ActDialog::Tunnel { key, local: String::new(), host: "127.0.0.1".into(), port: String::new(), error: String::new() });
        }
        let mut close = None;
        for tun in self.core.tunnels.iter().filter(|x| x.tab == key) {
            ui.horizontal(|ui| {
                let open = tun.open.load(std::sync::atomic::Ordering::Relaxed);
                ui.label(text(format!("127.0.0.1:{} → {}:{} · {open}", tun.local, tun.host, tun.port), 12.0, W::Regular, th::TEXT));
                if ui.small_button("✕").on_hover_text(t("tun.close")).clicked() {
                    close = Some(tun.id);
                }
            });
        }
        if let Some(id) = close {
            self.core.tunnel_close(id);
        }
        if ui.add_enabled(live, egui::Button::new(t("act.switch"))).on_hover_text(t("act.switchHint")).clicked() {
            self.core.switch_sides(key);
        }
        if ui.add_enabled(live && perms.restart, egui::Button::new(t("act.restart"))).on_disabled_hover_text(t("act.needPerm")).clicked() {
            self.act_dialog = Some(ActDialog::Restart { key });
        }
    }

    /// Saves the remote screen as it is now (full resolution) to Pictures.
    pub(super) fn screenshot(&mut self, key: u32) {
        let Some(pic) = self.last_pic.get(&key) else { return };
        let (w, h, rgba) = (pic.width as u32, pic.height as u32, pic.rgba.clone());
        let label = self.core.tabs.iter().find(|t| t.key == key).map(|t| t.label.clone()).unwrap_or_default();
        let dir = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|d| d.to_path_buf())).unwrap_or_else(crate::store::downloads);
        let name: String = format!("ManyDesk {} {}.png", label, chrono::Local::now().format("%Y-%m-%d %H.%M.%S")).chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
        let path = dir.join(name);
        crate::rt::spawn_blocking(move || {
            let r = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| "image".to_string()).and_then(|img| img.save(&path).map_err(|e| e.to_string()));
            match r {
                Ok(()) => bus::emit("toast", json!({ "text": tf("act.saved", &[("f", &path.to_string_lossy())]) })),
                Err(e) => bus::emit("toast", json!({ "text": e, "error": true })),
            }
        });
    }

    /// Whiteboard on the stage: drawing with the mouse instead of controlling.
    /// `rect` is where the remote screen is drawn. Returns true while the board is on.
    pub(super) fn board(&mut self, ui: &mut Ui, resp: &egui::Response, key: u32, rect: Rect, stage: Rect) -> bool {
        let Some(board) = self.boards.get_mut(&key) else { return false };
        let painter = ui.painter().with_clip_rect(stage);
        let to_screen = |(x, y): (f32, f32)| Pos2::new(rect.left() + x * rect.width(), rect.top() + y * rect.height());
        for (_, c, pts) in &board.strokes {
            let pts: Vec<Pos2> = pts.iter().map(|p| to_screen(*p)).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(3.0, Color32::from_rgb(c[0], c[1], c[2]))));
        }
        if !board.on {
            return false;
        }
        // the colour chips and the close chip, top right of the stage (on a phone a bit
        // larger and left of the corner)
        let big = super::compact(ui.ctx());
        let (size, step) = if big { (34.0, 40.0) } else { (22.0, 26.0) };
        let mut chip = Pos2::new(stage.right() - 14.0 - (BOARD_COLORS.len() + 1) as f32 * step, stage.top() + 12.0);
        let mut over_chips = false;
        for (n, c) in BOARD_COLORS.iter().enumerate() {
            let r = Rect::from_min_size(chip, Vec2::splat(size));
            let hit = ui.interact(r, Id::unique(("board-c", key, n)), Sense::click());
            over_chips |= hit.hovered() || hit.is_pointer_button_down_on();
            painter.circle(r.center(), size * 0.41, Color32::from_rgb(c[0], c[1], c[2]), Stroke::new(if board.color == n { 3.0 } else { 1.5 }, Color32::WHITE));
            if hit.clicked() {
                board.color = n;
            }
            chip.x += step;
        }
        let r = Rect::from_min_size(chip, Vec2::splat(size));
        let hit = ui.interact(r, Id::unique(("board-x", key)), Sense::click()).on_hover_text(t("board.close"));
        over_chips |= hit.hovered() || hit.is_pointer_button_down_on();
        painter.circle(r.center(), size * 0.45, th::SURFACE, Stroke::new(1.5, th::BORDER));
        let k = size * 0.17;
        for (a, b) in [(Vec2::new(-k, -k), Vec2::new(k, k)), (Vec2::new(-k, k), Vec2::new(k, -k))] {
            painter.line_segment([r.center() + a, r.center() + b], Stroke::new(2.0, th::TEXT));
        }
        if hit.clicked() {
            board.on = false;
            board.cur = None;
            return false;
        }
        ui.ctx().set_cursor_icon(if over_chips { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Crosshair });
        let norm = |p: Pos2| (((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
        let color = BOARD_COLORS[board.color];
        if resp.drag_started() && !over_chips {
            let id = rand::random::<u64>() >> 12;
            board.cur = Some(id);
            board.strokes.push((id, color, Vec::new()));
        }
        if let (Some(id), Some(p)) = (board.cur, resp.interact_pointer_pos().filter(|_| resp.dragged())) {
            let q = norm(p);
            if let Some(s) = board.strokes.iter_mut().rev().find(|s| s.0 == id) {
                if s.2.last().is_none_or(|l| (l.0 - q.0).abs() + (l.1 - q.1).abs() > 0.0008) {
                    s.2.push(q);
                    board.unsent.push(q);
                }
            }
        }
        let done = resp.drag_stopped();
        let due = board.last_send.is_none_or(|t| t.elapsed() > Duration::from_millis(40));
        if let Some(id) = board.cur.filter(|_| !board.unsent.is_empty() && (due || done)) {
            let pts = std::mem::take(&mut board.unsent);
            board.last_send = Some(Instant::now());
            self.core.send_draw(key, id, color, &pts);
        }
        if done {
            if let Some(b) = self.boards.get_mut(&key) {
                b.cur = None;
            }
        }
        true
    }

    /// Restart confirmation and the TCP tunnel dialog.
    pub(super) fn act_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.act_dialog.as_mut() else { return };
        let mut close = false;
        let mut submit = false;
        let modal = super::dialog_modal(ctx, "act-dialog")
            .frame(egui::Frame::new().fill(th::SURFACE).stroke(Stroke::new(1.0, th::BORDER)).corner_radius(12).shadow(th::shadow_lg()).inner_margin(Margin::same(22)))
            .show(ctx, |ui| {
                ui.set_width(super::dialog_width(ui.ctx(), 380.0));
                match d {
                    ActDialog::Restart { .. } => {
                        th::h3(ui, &t("act.restart"));
                        ui.label(text(t("act.restartAsk"), 13.0, W::Regular, th::TEXT));
                        ui.add_space(12.0);
                    }
                    ActDialog::Tunnel { local, host, port, error, .. } => {
                        th::h3(ui, &t("act.tunnel"));
                        th::hint(ui, &t("tun.hint"));
                        ui.add_space(8.0);
                        field(ui, &t("tun.local"), |ui| input(ui, local, false));
                        field(ui, &t("tun.host"), |ui| input(ui, host, false));
                        field(ui, &t("tun.port"), |ui| input(ui, port, false));
                        if !error.is_empty() {
                            ui.label(text(error.as_str(), 12.0, W::Regular, th::DANGER));
                            ui.add_space(6.0);
                        }
                    }
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let bw = (ui.available_width() - 8.0) / 2.0;
                    if th::btn_sized(ui, &t("btn.cancel"), Btn::Normal, Some(bw)).clicked() {
                        close = true;
                    }
                    let (label, kind) = match d {
                        ActDialog::Restart { .. } => (t("act.restartGo"), Btn::Danger),
                        ActDialog::Tunnel { .. } => (t("tun.open"), Btn::Primary),
                    };
                    if th::btn_sized(ui, &label, kind, Some(bw)).clicked() {
                        submit = true;
                    }
                });
            });
        if modal.should_close() {
            close = true;
        }
        if submit {
            match self.act_dialog.take() {
                Some(ActDialog::Restart { key }) => self.core.restart_remote(key),
                Some(ActDialog::Tunnel { key, local, host, port, .. }) => {
                    if let Err(e) = self.core.tunnel_open(key, local.trim(), host.trim(), port.trim()) {
                        self.act_dialog = Some(ActDialog::Tunnel { key, local, host, port, error: e });
                    }
                }
                None => {}
            }
        } else if close {
            self.act_dialog = None;
        }
    }

    /// Host: privacy mode blacks out every display for the people at this computer; the
    /// cover is kept out of the picture the viewers get.
    pub(super) fn privacy_cover(&mut self, ctx: &egui::Context) {
        if !self.core.privacy_on {
            self.covered.clear();
            return;
        }
        const TITLE: &str = "ManyDesk privacy";
        let screens = self.core.screens.clone();
        for s in screens {
            let (x, y, w, h) = crate::input::overlay_rect(&s);
            let title = format!("{TITLE} {}", s.id);
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_position([x, y])
                .with_inner_size([w, h])
                .with_decorations(false)
                .with_has_shadow(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_active(false)
                .with_mouse_passthrough(true)
                .with_window_level(egui::WindowLevel::AlwaysOnTop);
            let covered = &mut self.covered;
            let icons = &self.icons;
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("md-privacy", s.id)), builder, |ui, _| {
                let r = ui.max_rect();
                ui.painter().rect_filled(r, 0.0, Color32::BLACK);
                icons.paint(ui, "shield", Rect::from_center_size(r.center() - Vec2::new(0.0, 30.0), Vec2::splat(40.0)), Color32::from_gray(90));
                ui.painter().text(r.center() + Vec2::new(0.0, 18.0), Align2::CENTER_CENTER, t("priv.cover"), th::font(15.0, W::Medium), Color32::from_gray(140));
                if !covered.contains(&s.id) && crate::platform::cover_window(&title) {
                    covered.push(s.id);
                }
            });
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}
