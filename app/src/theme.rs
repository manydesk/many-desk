// The web UI's design system (src/renderer/styles.css) for egui: colour tokens, the OS
// UI font in its 400/500/600 weights plus the OS monospace font, the SVG icon sprite,
// and the small set of widgets the pages are built from (buttons, icon buttons,
// permission toggles, checkboxes, panels).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, Response, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2};

// ---- tokens (:root in styles.css) ----
pub const BG: Color32 = Color32::from_rgb(0xf3, 0xf4, 0xf6);
pub const SURFACE: Color32 = Color32::WHITE;
pub const SURFACE_2: Color32 = Color32::from_rgb(0xf7, 0xf7, 0xf8);
pub const BORDER: Color32 = Color32::from_rgb(0xe3, 0xe4, 0xe8);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0xcf, 0xd1, 0xd7);
pub const TEXT: Color32 = Color32::from_rgb(0x22, 0x25, 0x2b);
pub const MUTED: Color32 = Color32::from_rgb(0x6c, 0x72, 0x7f);
pub const ACCENT: Color32 = Color32::from_rgb(0xe5, 0x46, 0x3b);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xd2, 0x3a, 0x30);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xfd, 0xec, 0xea);
pub const OK: Color32 = Color32::from_rgb(0x1f, 0x9d, 0x55);
pub const OK_SOFT: Color32 = Color32::from_rgb(0xe9, 0xf7, 0xef);
pub const WARN: Color32 = Color32::from_rgb(0xd9, 0x8b, 0x0b);
pub const DANGER: Color32 = Color32::from_rgb(0xd3, 0x3a, 0x2f);
pub const STAGE: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x22);
pub const TOAST: Color32 = Color32::from_rgb(0x2b, 0x2f, 0x36);
pub const RADIUS: u8 = 8;

/// --shadow: 0 1px 3px rgba(16,24,40,.08)
pub fn shadow() -> Shadow {
    Shadow { offset: [0, 1], blur: 3, spread: 0, color: Color32::from_rgba_unmultiplied(16, 24, 40, 20) }
}

/// --shadow-lg: 0 16px 48px rgba(16,24,40,.22)
pub fn shadow_lg() -> Shadow {
    Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_rgba_unmultiplied(16, 24, 40, 56) }
}

// ---- fonts ----

#[derive(Clone, Copy, PartialEq)]
pub enum W {
    Regular,
    Medium,
    Semibold,
}

pub fn font(size: f32, w: W) -> FontId {
    FontId::new(
        size,
        match w {
            W::Regular => FontFamily::Proportional,
            W::Medium => FontFamily::Name("w500".into()),
            W::Semibold => FontFamily::Name("w600".into()),
        },
    )
}

pub fn mono(size: f32, w: W) -> FontId {
    FontId::new(size, if w == W::Regular { FontFamily::Monospace } else { FontFamily::Name("mono600".into()) })
}

struct Face {
    path: &'static str,
    index: u32,
    /// variable fonts: the `wght` to use; static files: None
    wght: Option<f32>,
}

const fn f(path: &'static str, wght: Option<f32>) -> Face {
    Face { path, index: 0, wght }
}

/// (regular, medium, semibold, mono, mono-semibold) candidates per platform, first that exists wins.
fn candidates() -> [Vec<Face>; 5] {
    #[cfg(target_os = "macos")]
    return [
        vec![f("/System/Library/Fonts/SFNS.ttf", Some(400.0))],
        vec![f("/System/Library/Fonts/SFNS.ttf", Some(510.0))],
        vec![f("/System/Library/Fonts/SFNS.ttf", Some(590.0))],
        vec![f("/System/Library/Fonts/SFNSMono.ttf", Some(400.0))],
        vec![f("/System/Library/Fonts/SFNSMono.ttf", Some(600.0))],
    ];
    #[cfg(windows)]
    return [
        vec![f("C:\\Windows\\Fonts\\segoeui.ttf", None)],
        vec![f("C:\\Windows\\Fonts\\seguisb.ttf", None), f("C:\\Windows\\Fonts\\segoeui.ttf", None)],
        vec![f("C:\\Windows\\Fonts\\seguisb.ttf", None), f("C:\\Windows\\Fonts\\segoeuib.ttf", None)],
        vec![f("C:\\Windows\\Fonts\\consola.ttf", None)],
        vec![f("C:\\Windows\\Fonts\\consolab.ttf", None), f("C:\\Windows\\Fonts\\consola.ttf", None)],
    ];
    // Roboto is a variable font from Android 12 on; older phones have the static files.
    #[cfg(target_os = "android")]
    return [
        vec![f("/system/fonts/Roboto-Regular.ttf", Some(400.0))],
        vec![f("/system/fonts/Roboto-Regular.ttf", Some(500.0)), f("/system/fonts/Roboto-Medium.ttf", None)],
        vec![f("/system/fonts/Roboto-Regular.ttf", Some(600.0)), f("/system/fonts/Roboto-Bold.ttf", None)],
        vec![f("/system/fonts/DroidSansMono.ttf", None), f("/system/fonts/CutiveMono.ttf", None)],
        vec![f("/system/fonts/DroidSansMono.ttf", None), f("/system/fonts/CutiveMono.ttf", None)],
    ];
    // iOS: the system fonts, where the sandbox lets them be read (egui's own otherwise)
    #[cfg(target_os = "ios")]
    return [
        vec![f("/System/Library/Fonts/Core/SFUI.ttf", Some(400.0))],
        vec![f("/System/Library/Fonts/Core/SFUI.ttf", Some(510.0))],
        vec![f("/System/Library/Fonts/Core/SFUI.ttf", Some(590.0))],
        vec![f("/System/Library/Fonts/Core/SFUIMono.ttf", Some(400.0))],
        vec![f("/System/Library/Fonts/Core/SFUIMono.ttf", Some(600.0))],
    ];
    #[cfg(target_os = "linux")]
    return [
        vec![
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu[wdth,wght].ttf", Some(400.0)),
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf", None),
            f("/usr/share/fonts/cantarell/Cantarell-VF.otf", Some(400.0)),
            f("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", None),
            f("/usr/share/fonts/TTF/DejaVuSans.ttf", None),
        ],
        vec![
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu[wdth,wght].ttf", Some(500.0)),
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu-M.ttf", None),
            f("/usr/share/fonts/cantarell/Cantarell-VF.otf", Some(500.0)),
            f("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", None),
        ],
        vec![
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu[wdth,wght].ttf", Some(600.0)),
            f("/usr/share/fonts/truetype/ubuntu/Ubuntu-B.ttf", None),
            f("/usr/share/fonts/cantarell/Cantarell-VF.otf", Some(650.0)),
            f("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", None),
            f("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf", None),
        ],
        vec![
            f("/usr/share/fonts/truetype/ubuntu/UbuntuMono[wght].ttf", Some(400.0)),
            f("/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf", None),
            f("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", None),
            f("/usr/share/fonts/TTF/DejaVuSansMono.ttf", None),
        ],
        vec![
            f("/usr/share/fonts/truetype/ubuntu/UbuntuMono[wght].ttf", Some(700.0)),
            f("/usr/share/fonts/truetype/ubuntu/UbuntuMono-B.ttf", None),
            f("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", None),
            f("/usr/share/fonts/TTF/DejaVuSansMono-Bold.ttf", None),
        ],
    ];
}

pub fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let names = ["ui400", "ui500", "ui600", "mono400", "mono600"];
    let mut cache: HashMap<&str, Arc<Vec<u8>>> = HashMap::new();
    let mut found = [false; 5];
    for (i, list) in candidates().into_iter().enumerate() {
        for face in list {
            let bytes = match cache.get(face.path) {
                Some(b) => b.clone(),
                None => match std::fs::read(face.path) {
                    Ok(b) => {
                        let b = Arc::new(b);
                        cache.insert(face.path, b.clone());
                        b
                    }
                    Err(_) => continue,
                },
            };
            let mut data = egui::FontData::from_owned((*bytes).clone());
            data.index = face.index;
            if let Some(w) = face.wght {
                let mut tweak = egui::FontTweak::default();
                tweak.coords = if face.path.ends_with("SFNS.ttf") {
                    // optical size 17 = "SF Pro Text", what WebKit uses for UI sizes
                    egui::epaint::text::VariationCoords::new([("wght", w), ("opsz", 17.0)])
                } else {
                    egui::epaint::text::VariationCoords::new([("wght", w)])
                };
                data = data.tweak(tweak);
            }
            fonts.font_data.insert(names[i].into(), Arc::new(data));
            found[i] = true;
            break;
        }
    }
    let fam = |f: &mut egui::FontDefinitions, family: FontFamily, first: &str, base: FontFamily| {
        let mut list = f.families.get(&base).cloned().unwrap_or_default();
        if f.font_data.contains_key(first) {
            list.insert(0, first.to_string());
        }
        f.families.insert(family, list);
    };
    fam(&mut fonts, FontFamily::Proportional, "ui400", FontFamily::Proportional);
    fam(&mut fonts, FontFamily::Name("w500".into()), "ui500", FontFamily::Proportional);
    fam(&mut fonts, FontFamily::Name("w600".into()), "ui600", FontFamily::Proportional);
    fam(&mut fonts, FontFamily::Monospace, "mono400", FontFamily::Monospace);
    fam(&mut fonts, FontFamily::Name("mono600".into()), "mono600", FontFamily::Monospace);
    // Android's Roboto has no arrows and few symbols: Noto's symbol fonts behind every family
    #[cfg(target_os = "android")]
    for (n, path) in ["/system/fonts/NotoSansSymbols-Regular-Subsetted.ttf", "/system/fonts/NotoSansSymbols-Regular-Subsetted2.ttf"].into_iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = format!("sym{n}");
        fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        for list in fonts.families.values_mut() {
            list.push(name.clone());
        }
    }
    ctx.set_fonts(fonts);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = SURFACE;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.selection.bg_fill = ACCENT_SOFT;
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.text_cursor.stroke = Stroke::new(1.5, ACCENT);
    style.visuals.extreme_bg_color = SURFACE_2;
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.window_corner_radius = CornerRadius::same(12);
    style.visuals.menu_corner_radius = CornerRadius::same(8);
    style.visuals.window_shadow = shadow_lg();
    style.visuals.popup_shadow = shadow_lg();
    for w in [&mut style.visuals.widgets.inactive, &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active, &mut style.visuals.widgets.open, &mut style.visuals.widgets.noninteractive] {
        w.corner_radius = CornerRadius::same(6);
        w.fg_stroke.color = TEXT;
    }
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE;
    style.visuals.widgets.inactive.bg_fill = SURFACE;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_2;
    style.visuals.widgets.hovered.bg_fill = SURFACE_2;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    style.visuals.widgets.active.weak_bg_fill = SURFACE_2;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    use egui::TextStyle::*;
    style.text_styles = [
        (Small, font(11.5, W::Regular)),
        (Body, font(13.5, W::Regular)),
        (Button, font(13.5, W::Medium)),
        (Heading, font(19.0, W::Semibold)),
        (Monospace, mono(12.0, W::Regular)),
    ]
    .into();
    ctx.set_global_style(style);
}

// ---- icons (the web sprite, rasterized with resvg) ----

#[derive(Default)]
pub struct Icons {
    cache: Mutex<HashMap<(&'static str, u32), TextureHandle>>,
}

impl Icons {
    fn texture(&self, ctx: &egui::Context, name: &'static str, px: u32) -> Option<TextureHandle> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(t) = cache.get(&(name, px)) {
            return Some(t.clone());
        }
        let svg = format!("{}{}</svg>", crate::sprite::SVG_HEAD, crate::sprite::inner(name));
        let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
        let mut pix = resvg::tiny_skia::Pixmap::new(px, px)?;
        let s = px as f32 / 24.0;
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pix.as_mut());
        let img = egui::ColorImage::from_rgba_premultiplied([px as usize, px as usize], pix.data());
        let tex = ctx.load_texture(format!("icon-{name}-{px}"), img, TextureOptions::LINEAR);
        cache.insert((name, px), tex.clone());
        Some(tex)
    }

    /// Paints icon `name` (sprite id without "i-") into `rect`, tinted `color`.
    pub fn paint(&self, ui: &Ui, name: &'static str, rect: Rect, color: Color32) {
        let px = (rect.width() * ui.ctx().pixels_per_point()).round().max(8.0) as u32;
        if let Some(t) = self.texture(ui.ctx(), name, px) {
            ui.painter().image(t.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), color);
        }
    }

    pub fn show(&self, ui: &mut Ui, name: &'static str, size: f32, color: Color32) -> Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        self.paint(ui, name, rect, color);
        resp
    }
}

// ---- widgets ----

#[derive(Clone, Copy, PartialEq)]
pub enum Btn {
    Normal,
    Primary,
    Danger,
}

/// .btn / .btn.primary / .btn.danger
pub fn btn(ui: &mut Ui, text: &str, kind: Btn) -> Response {
    btn_sized(ui, text, kind, None)
}

pub fn btn_sized(ui: &mut Ui, text: &str, kind: Btn, width: Option<f32>) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(13.5, W::Medium), TEXT);
    let size = Vec2::new(width.unwrap_or(galley.size().x + 28.0), 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hover = resp.hovered();
    let (fill, stroke, fg) = match kind {
        Btn::Normal => (if hover { SURFACE_2 } else { SURFACE }, BORDER_STRONG, TEXT),
        Btn::Primary => (if hover { ACCENT_HOVER } else { ACCENT }, if hover { ACCENT_HOVER } else { ACCENT }, Color32::WHITE),
        Btn::Danger => {
            if hover {
                (DANGER, DANGER, Color32::WHITE)
            } else {
                (SURFACE, mix(DANGER, BORDER, 0.45), DANGER)
            }
        }
    };
    ui.painter().rect(rect, 6.0, fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(text.to_string(), font(13.5, W::Medium), fg);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, fg);
    if hover {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// .iconbtn: 32×32, muted icon that darkens on hover.
pub fn iconbtn(ui: &mut Ui, icons: &Icons, name: &'static str, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let hover = resp.hovered();
    if hover {
        ui.painter().rect_filled(rect, 6.0, SURFACE_2);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    icons.paint(ui, name, Rect::from_center_size(rect.center(), Vec2::splat(18.0)), if hover { TEXT } else { MUTED });
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// .tbbtn: 34×32 toolbar button; `on` = accent-soft state, `danger` = red power button.
pub fn tbbtn(ui: &mut Ui, icons: &Icons, name: &'static str, on: bool, danger: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 32.0), Sense::click());
    let hover = resp.hovered();
    let (bg, fg) = if danger {
        if hover { (Some(DANGER), Color32::WHITE) } else { (None, DANGER) }
    } else if on {
        (Some(ACCENT_SOFT), ACCENT)
    } else if hover {
        (Some(SURFACE_2), TEXT)
    } else {
        (None, TEXT)
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 6.0, bg);
    }
    if hover {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    icons.paint(ui, name, Rect::from_center_size(rect.center(), Vec2::splat(18.0)), fg);
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// A toolbar display button: a monitor with its number on the screen (AnyDesk style).
pub fn screenbtn(ui: &mut Ui, icons: &Icons, n: usize, on: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 32.0), Sense::click());
    let hover = resp.hovered();
    let (bg, fg) = if on { (Some(ACCENT_SOFT), ACCENT) } else if hover { (Some(SURFACE_2), TEXT) } else { (None, TEXT) };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 6.0, bg);
    }
    if hover {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let icon = Rect::from_center_size(rect.center(), Vec2::splat(22.0));
    icons.paint(ui, "monitor", icon, fg);
    // the screen part of the 24-unit icon is centred at (12, 10)
    let at = icon.center() + Vec2::new(0.0, -2.0 * 22.0 / 24.0);
    ui.painter().text(at, egui::Align2::CENTER_CENTER, n.to_string(), font(9.5, W::Semibold), fg);
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// The red count bubble on a toolbar button (.unread).
pub fn badge(ui: &Ui, anchor: Rect, n: usize) {
    if n == 0 {
        return;
    }
    let text = n.to_string();
    let g = ui.painter().layout_no_wrap(text, font(10.5, W::Semibold), Color32::WHITE);
    let w = (g.size().x + 8.0).max(16.0);
    let r = Rect::from_min_size(Pos2::new(anchor.right() - w + 3.0, anchor.top() - 3.0), Vec2::new(w, 16.0));
    ui.painter().rect_filled(r, 8.0, ACCENT);
    ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
}

/// .permtoggle: icon + label chip; accent when checked.
pub fn permtoggle(ui: &mut Ui, icons: &Icons, icon: &'static str, label: &str, on: &mut bool) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_string(), font(12.5, W::Regular), MUTED);
    let size = Vec2::new(g.size().x + 11.0 + 18.0 + 7.0 + 11.0, 32.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let (bg, stroke, fg) = if *on { (ACCENT_SOFT, ACCENT, ACCENT) } else { (Color32::TRANSPARENT, BORDER_STRONG, MUTED) };
    ui.painter().rect(rect, 6.0, bg, Stroke::new(1.0, stroke), StrokeKind::Inside);
    icons.paint(ui, icon, Rect::from_min_size(Pos2::new(rect.left() + 11.0, rect.center().y - 9.0), Vec2::splat(18.0)), fg);
    let g = ui.painter().layout_no_wrap(label.to_string(), font(12.5, W::Regular), fg);
    ui.painter().galley(Pos2::new(rect.left() + 11.0 + 18.0 + 7.0, rect.center().y - g.size().y / 2.0), g, fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// input[type=checkbox] with accent-color, and its label.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let g = ui.painter().layout_no_wrap(label.to_string(), font(13.5, W::Regular), TEXT);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(15.0 + 8.0 + g.size().x, g.size().y.max(18.0)), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let b = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 7.5), Vec2::splat(15.0));
    if *on {
        ui.painter().rect_filled(b, 3.0, ACCENT);
        let s = Stroke::new(2.0, Color32::WHITE);
        ui.painter().line_segment([b.lerp_inside(Vec2::new(0.25, 0.52)), b.lerp_inside(Vec2::new(0.43, 0.7))], s);
        ui.painter().line_segment([b.lerp_inside(Vec2::new(0.43, 0.7)), b.lerp_inside(Vec2::new(0.76, 0.32))], s);
    } else {
        ui.painter().rect(b, 3.0, SURFACE, Stroke::new(1.0, Color32::from_gray(0x76)), StrokeKind::Inside);
    }
    ui.painter().galley(Pos2::new(b.right() + 8.0, rect.center().y - g.size().y / 2.0), g, TEXT);
    resp
}

/// .panel: white card, border, radius 8, soft shadow, padding 16×18.
pub fn panel<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(RADIUS)
        .shadow(shadow())
        .inner_margin(Margin { left: 18, right: 18, top: 16, bottom: 16 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(14.0);
    r
}

/// .panel h3
pub fn h3(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(font(14.0, W::Semibold)).line_height(Some(20.0)));
    ui.add_space(4.0);
}

/// .hint
pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(font(12.5, W::Regular)).color(MUTED).line_height(Some(18.0)));
}

/// .dot (8 px status dot)
pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// .pulse: an 8 px green dot with an expanding ring.
pub fn pulse(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    let t = (ui.input(|i| i.time) % 1.8) as f32 / 1.8;
    if t < 0.7 {
        let k = t / 0.7;
        ui.painter().circle_filled(rect.center(), 4.0 + 8.0 * k, OK.gamma_multiply(0.5 * (1.0 - k)));
    }
    ui.painter().circle_filled(rect.center(), 4.0, OK);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
}

/// .spinner: 38 px ring with a rotating accent arc.
pub fn spinner(ui: &mut Ui, stopped: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
    let c = rect.center();
    let r = 17.5;
    ui.painter().circle_stroke(c, r, Stroke::new(3.0, if stopped { BORDER } else { ACCENT_SOFT }));
    if stopped {
        return;
    }
    let a0 = (ui.input(|i| i.time) % 0.9) as f32 / 0.9 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2 - 0.78;
    let pts: Vec<Pos2> = (0..=16).map(|i| c + Vec2::angled(a0 + i as f32 * std::f32::consts::FRAC_PI_2 / 16.0) * r).collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(3.0, ACCENT)));
    ui.ctx().request_repaint();
}

/// A dashed rounded rectangle (.empty's 1.5 px dashed border).
pub fn dashed_rect(ui: &Ui, rect: Rect, color: Color32) {
    let s = Stroke::new(1.5, color);
    let p = ui.painter();
    for (a, b) in [(rect.left_top(), rect.right_top()), (rect.right_top(), rect.right_bottom()), (rect.right_bottom(), rect.left_bottom()), (rect.left_bottom(), rect.left_top())] {
        p.extend(egui::Shape::dashed_line(&[a, b], s, 5.0, 4.0));
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
