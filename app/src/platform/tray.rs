// ManyDesk's icon in the tray (Windows), the menu bar (macOS) or the status area (Linux,
// StatusNotifierItem). Closing the window leaves ManyDesk running there, as AnyDesk does:
// the computer stays reachable, and the icon brings the window back or quits. Where no
// icon could be put up (a Linux desktop without a status area), closing quits as before.
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

/// The icon asked for the window (a click, "Open ManyDesk", the Dock icon on macOS).
pub static SHOW: AtomicBool = AtomicBool::new(false);
/// "Quit" in the icon's menu: the window may close for real.
pub static QUIT: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// An icon is up: closing the window hides it instead of quitting.
pub fn active() -> bool {
    ACTIVE.load(SeqCst)
}

#[cfg_attr(mobile, allow(dead_code))]
fn request_show() {
    SHOW.store(true, SeqCst);
    crate::bus::wake();
}

#[cfg_attr(mobile, allow(dead_code))]
fn request_quit() {
    QUIT.store(true, SeqCst);
    crate::bus::wake();
}

/// The app icon as RGBA, `size` pixels square.
#[cfg_attr(mobile, allow(dead_code))]
fn rgba(size: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(include_bytes!("../../assets/icon.png")).ok()?;
    Some(img.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8().into_raw())
}

/// The monitor of the app's icon set, black on clear (a menu-bar template image).
#[cfg(target_os = "macos")]
fn glyph(size: u32) -> Option<Vec<u8>> {
    let svg = format!("{}{}</svg>", crate::sprite::SVG_HEAD.replace("#ffffff", "#000000").replace("1.8", "2"), crate::sprite::inner("monitor"));
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pix = resvg::tiny_skia::Pixmap::new(size, size)?;
    let s = size as f32 / 24.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pix.as_mut());
    Some(pix.take())
}

#[cfg(windows)]
fn glyph(_: u32) -> Option<Vec<u8>> {
    None
}

/// Puts the icon up. Windows and macOS: on the UI thread, inside the event loop (App::new).
#[cfg(any(windows, target_os = "macos"))]
pub fn start() {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let menu = Menu::new();
    let open = MenuItem::new(crate::i18n::t("tray.open"), true, None);
    let quit = MenuItem::new(crate::i18n::t("tray.quit"), true, None);
    if menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]).is_err() {
        return;
    }
    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            request_show();
        } else if e.id == quit_id {
            request_quit();
        }
    }));
    // Windows: a left click opens the window, the right one the menu (as AnyDesk's icon)
    TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
        if matches!(e, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { .. }) {
            request_show();
        }
    }));
    // macOS: a template image (the menu bar tints it for light and dark, like its other
    // icons); Windows: the app's own icon
    let size = if cfg!(target_os = "macos") { 36 } else { 32 };
    let pixels = if cfg!(target_os = "macos") { glyph(size) } else { rgba(size) };
    let Some(icon) = pixels.and_then(|d| tray_icon::Icon::from_rgba(d, size, size).ok()) else { return };
    let builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("ManyDesk").with_menu_on_left_click(cfg!(target_os = "macos"));
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon);
    #[cfg(windows)]
    let builder = builder.with_icon(icon);
    match builder.build() {
        Ok(tray) => {
            // kept for the life of the process
            std::mem::forget(tray);
            ACTIVE.store(true, SeqCst);
            #[cfg(target_os = "macos")]
            dock_reopen();
        }
        Err(e) => eprintln!("[tray] {e}"),
    }
}

/// macOS: a click on the Dock icon (or opening ManyDesk again) while the window is hidden
/// brings it back. winit's application delegate does not answer that, so the method is
/// added to its class.
#[cfg(target_os = "macos")]
fn dock_reopen() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::{msg_send, sel};
    extern "C-unwind" fn reopen(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, _visible: Bool) -> Bool {
        request_show();
        Bool::YES
    }
    unsafe {
        let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if delegate.is_null() {
            return;
        }
        let cls: *const AnyClass = objc2::ffi::object_getClass(delegate);
        let imp: unsafe extern "C-unwind" fn() = std::mem::transmute(reopen as extern "C-unwind" fn(_, _, _, _) -> _);
        // returns BOOL, takes (self, _cmd, NSApplication*, BOOL)
        let added = objc2::ffi::class_addMethod(cls as *mut AnyClass, sel!(applicationShouldHandleReopen:hasVisibleWindows:), imp, c"c@:@c".as_ptr());
        if !added.as_bool() {
            eprintln!("[tray] the Dock icon does not reopen the window");
        }
    }
}

/// Linux: a StatusNotifierItem over D-Bus (KDE, and GNOME with the AppIndicator extension,
/// as Ubuntu has it). No icon without a status area: then closing quits.
#[cfg(target_os = "linux")]
pub fn start() {
    use ksni::menu::StandardItem;
    struct Tray(Vec<ksni::Icon>);
    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "manydesk".into()
        }
        fn title(&self) -> String {
            "ManyDesk".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.0.clone()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            request_show();
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                StandardItem { label: crate::i18n::t("tray.open"), activate: Box::new(|_: &mut Self| request_show()), ..Default::default() }.into(),
                ksni::MenuItem::Separator,
                StandardItem { label: crate::i18n::t("tray.quit"), activate: Box::new(|_: &mut Self| request_quit()), ..Default::default() }.into(),
            ]
        }
    }
    let Some(rgba) = rgba(32) else { return };
    // ARGB32, network byte order
    let data = rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    let tray = Tray(vec![ksni::Icon { width: 32, height: 32, data }]);
    crate::rt::spawn(async move {
        use ksni::TrayMethods;
        match tray.spawn().await {
            Ok(handle) => {
                std::mem::forget(handle);
                ACTIVE.store(true, SeqCst);
            }
            Err(e) => eprintln!("[tray] no status area: {e}"),
        }
    });
}

#[cfg(mobile)]
#[allow(dead_code)]
pub fn start() {}
