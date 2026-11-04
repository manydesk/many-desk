// The remote terminal's window: it takes the stage while it is open (as the file manager
// does). The screen is vt100's; drawn cell by cell in the monospace font, the keys go to
// the shell as a terminal sends them (xterm).
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use super::*;

const FONT: f32 = 13.0;
const BG: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x24);
const FG: Color32 = Color32::from_rgb(0xd8, 0xdb, 0xe0);

/// xterm's 16 colours (as most terminal apps show them).
const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x1b, 0x1e, 0x24),
    Color32::from_rgb(0xe0, 0x6c, 0x75),
    Color32::from_rgb(0x98, 0xc3, 0x79),
    Color32::from_rgb(0xe5, 0xc0, 0x7b),
    Color32::from_rgb(0x61, 0xaf, 0xef),
    Color32::from_rgb(0xc6, 0x78, 0xdd),
    Color32::from_rgb(0x56, 0xb6, 0xc2),
    Color32::from_rgb(0xd8, 0xdb, 0xe0),
    Color32::from_rgb(0x5c, 0x63, 0x70),
    Color32::from_rgb(0xff, 0x7b, 0x86),
    Color32::from_rgb(0xb5, 0xe0, 0x96),
    Color32::from_rgb(0xff, 0xd8, 0x8f),
    Color32::from_rgb(0x82, 0xc4, 0xff),
    Color32::from_rgb(0xdf, 0x98, 0xf0),
    Color32::from_rgb(0x7f, 0xd8, 0xe4),
    Color32::from_rgb(0xff, 0xff, 0xff),
];

fn color(c: vt100::Color, default: Color32) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) if i < 16 => ANSI[i as usize],
        // 6×6×6 cube, then 24 greys
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let v = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            Color32::from_rgb(v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        vt100::Color::Idx(i) => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// What a key sends to the shell (None: not a terminal key).
fn key_bytes(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key as K;
    let csi = |s: &str| Some(format!("\x1b{}{s}", if app_cursor { "O" } else { "[" }).into_bytes());
    // Ctrl+letter: its control code (Ctrl+C interrupts, Ctrl+D ends…); Cmd on a Mac stays ours
    if m.ctrl && !m.alt {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(vec![c - b'@']);
            }
        }
        match key {
            K::OpenBracket => return Some(vec![0x1b]),
            K::Backslash => return Some(vec![0x1c]),
            K::CloseBracket => return Some(vec![0x1d]),
            _ => {}
        }
    }
    match key {
        K::Enter => Some(b"\r".to_vec()),
        K::Backspace => Some(vec![0x7f]),
        K::Tab if m.shift => Some(b"\x1b[Z".to_vec()),
        K::Tab => Some(b"\t".to_vec()),
        K::Escape => Some(vec![0x1b]),
        K::ArrowUp => csi("A"),
        K::ArrowDown => csi("B"),
        K::ArrowRight => csi("C"),
        K::ArrowLeft => csi("D"),
        K::Home => csi("H"),
        K::End => csi("F"),
        K::PageUp => Some(b"\x1b[5~".to_vec()),
        K::PageDown => Some(b"\x1b[6~".to_vec()),
        K::Delete => Some(b"\x1b[3~".to_vec()),
        K::Insert => Some(b"\x1b[2~".to_vec()),
        K::F1 => Some(b"\x1bOP".to_vec()),
        K::F2 => Some(b"\x1bOQ".to_vec()),
        K::F3 => Some(b"\x1bOR".to_vec()),
        K::F4 => Some(b"\x1bOS".to_vec()),
        _ => None,
    }
}

impl App {
    /// The toolbar's terminal button: shows the terminal (a new shell if there is none, or
    /// the last one ended) or hides it (the shell keeps running).
    pub(super) fn toggle_term(&mut self, key: u32) {
        let Some(tab) = self.core.tab(key) else { return };
        tab.term_open = !tab.term_open;
        if tab.term_open {
            tab.fm.open = false;
            if tab.term.as_ref().is_some_and(|t| t.ended.is_some()) {
                tab.term = None;
            }
            self.core.set_capture(None);
        }
    }

    /// The terminal of session `key` in `stage`.
    pub(super) fn terminal(&mut self, ui: &mut Ui, stage: Rect, key: u32) {
        let font = FontId::monospace(FONT);
        let cell = {
            let g = ui.painter().layout_no_wrap("M".into(), font.clone(), FG);
            Vec2::new(g.size().x, (FONT * 1.3).round())
        };
        // the title bar
        let bar = Rect::from_min_size(stage.min, Vec2::new(stage.width(), 36.0));
        let p = ui.painter().clone();
        p.rect_filled(bar, 0.0, th::TOAST);
        self.icons.paint(ui, "terminal", Rect::from_center_size(Pos2::new(bar.left() + 20.0, bar.center().y), Vec2::splat(16.0)), Color32::WHITE);
        let label = self.core.tabs.iter().find(|t| t.key == key).map(|t| t.label.clone()).unwrap_or_default();
        p.text(Pos2::new(bar.left() + 38.0, bar.center().y), Align2::LEFT_CENTER, format!("{} — {label}", t("term.title")), th::font(13.0, W::Semibold), Color32::WHITE);
        let close = Rect::from_center_size(Pos2::new(bar.right() - 20.0, bar.center().y), Vec2::splat(26.0));
        let cr = ui.interact(close, Id::unique(("term-close", key)), Sense::click()).on_hover_text(t("term.close"));
        if cr.hovered() {
            p.circle_filled(close.center(), 13.0, Color32::from_white_alpha(30));
        }
        self.icons.paint(ui, "x", Rect::from_center_size(close.center(), Vec2::splat(14.0)), Color32::WHITE);
        if cr.clicked() {
            self.core.term_close(key);
            return;
        }
        // the screen
        let area = Rect::from_min_max(Pos2::new(stage.left(), bar.bottom()), stage.max);
        p.rect_filled(area, 0.0, BG);
        let inner = area.shrink(8.0);
        let rows = ((inner.height() / cell.y).floor() as u16).max(2);
        let cols = ((inner.width() / cell.x).floor() as u16).max(2);
        // the shell starts once the host allows it (it may tick Terminal during the session)
        let allowed = self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| t.remote.terminal);
        if !allowed {
            let g = p.layout(t("term.waitPerm"), th::font(14.0, W::Medium), FG, (area.width() - 80.0).max(120.0));
            p.galley(Pos2::new(area.center().x - g.size().x / 2.0, area.center().y - g.size().y / 2.0), g, FG);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        if self.core.tabs.iter().find(|t| t.key == key).is_some_and(|t| t.term.is_none()) {
            self.core.term_open(key, rows, cols);
        }
        let resp = ui.interact(area, Id::unique(("term", key)), Sense::click());
        if resp.clicked() {
            resp.request_focus();
        }
        // keys go to the shell while the window is open (it has the whole stage)
        let events = ui.input(|i| i.events.clone());
        let scroll = ui.input(|i| if area.contains(i.pointer.hover_pos().unwrap_or_default()) { i.smooth_scroll_delta.y } else { 0.0 });
        let Some(tab) = self.core.tab(key) else { return };
        let Some(term) = tab.term.as_mut() else { return };
        term.resize(rows, cols);
        let app_cursor = term.parser.screen().application_cursor();
        let bracketed = term.parser.screen().bracketed_paste();
        let mut out = Vec::new();
        let mut copy = false;
        let focused = resp.has_focus();
        for ev in events.iter().filter(|_| focused) {
            match ev {
                // Ctrl+C / Ctrl+X come as Copy and Cut where Ctrl is the command key: there
                // they interrupt, as in a terminal; Cmd+C on a Mac copies the screen
                egui::Event::Copy if cfg!(target_os = "macos") => copy = true,
                egui::Event::Copy => out.push(0x03),
                egui::Event::Cut if !cfg!(target_os = "macos") => out.push(0x18),
                egui::Event::Text(s) => out.extend_from_slice(s.as_bytes()),
                egui::Event::Paste(s) => {
                    if bracketed {
                        out.extend_from_slice(b"\x1b[200~");
                    }
                    out.extend_from_slice(s.replace('\n', "\r").as_bytes());
                    if bracketed {
                        out.extend_from_slice(b"\x1b[201~");
                    }
                }
                egui::Event::Key { key: k, pressed: true, modifiers, .. } => {
                    if let Some(b) = key_bytes(*k, *modifiers, app_cursor) {
                        // Alt+key: ESC first (as "meta sends escape")
                        if modifiers.alt && b.len() == 1 {
                            out.push(0x1b);
                        }
                        out.extend(b);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            // typing brings the view back down
            term.parser.screen_mut().set_scrollback(0);
            term.send_keys(&out);
        }
        if scroll != 0.0 {
            let back = term.parser.screen().scrollback() as i64 + (scroll / cell.y).round() as i64;
            term.parser.screen_mut().set_scrollback(back.max(0) as usize);
        }
        let screen = term.parser.screen();
        if copy {
            ui.ctx().copy_text(screen.contents());
        }
        let (srows, scols) = screen.size();
        for r in 0..srows {
            let y = inner.top() + r as f32 * cell.y;
            for c in 0..scols {
                let Some(cl) = screen.cell(r, c) else { continue };
                if cl.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (color(cl.fgcolor(), FG), color(cl.bgcolor(), BG));
                if cl.bold() {
                    if let vt100::Color::Idx(i) = cl.fgcolor() {
                        if i < 8 {
                            fg = ANSI[i as usize + 8];
                        }
                    }
                }
                if cl.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let x = inner.left() + c as f32 * cell.x;
                let w = if cl.is_wide() { cell.x * 2.0 } else { cell.x };
                if bg != BG {
                    p.rect_filled(Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, cell.y)), 0.0, bg);
                }
                let s = cl.contents();
                if !s.is_empty() && s != " " {
                    p.text(Pos2::new(x, y + cell.y / 2.0), Align2::LEFT_CENTER, s, font.clone(), if cl.dim() { fg.gamma_multiply(0.6) } else { fg });
                }
                if cl.underline() {
                    p.hline(x..=x + w, y + cell.y - 2.0, Stroke::new(1.0, fg));
                }
            }
        }
        // the cursor (only at the bottom of the scrollback)
        if !screen.hide_cursor() && screen.scrollback() == 0 && term.ended.is_none() {
            let (cr, cc) = screen.cursor_position();
            let r = Rect::from_min_size(Pos2::new(inner.left() + cc as f32 * cell.x, inner.top() + cr as f32 * cell.y), cell);
            if resp.has_focus() {
                p.rect_filled(r, 0.0, FG.gamma_multiply(0.7));
                if let Some(cl) = screen.cell(cr, cc) {
                    p.text(Pos2::new(r.left(), r.center().y), Align2::LEFT_CENTER, cl.contents(), font.clone(), BG);
                }
            } else {
                p.rect_stroke(r, 0.0, Stroke::new(1.0, FG), StrokeKind::Inside);
            }
        }
        if let Some(e) = term.ended.clone() {
            let msg = if e.is_empty() { t("term.ended") } else { format!("{}: {e}", t("term.failed")) };
            let g = p.layout_no_wrap(msg, th::font(13.0, W::Medium), Color32::WHITE);
            let r = Rect::from_center_size(Pos2::new(area.center().x, area.bottom() - 40.0), g.size() + Vec2::new(28.0, 16.0));
            p.rect_filled(r, 8.0, th::TOAST);
            p.galley(r.min + Vec2::new(14.0, 8.0), g, Color32::WHITE);
        }
        // keep the keys here: the shell has them, not the remote screen or egui's widgets
        if resp.has_focus() || !ui.ctx().memory(|m| m.focused().is_some()) {
            resp.request_focus();
            ui.ctx().memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
}
