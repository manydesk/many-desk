// ManyDesk — native remote desktop. egui UI on top of the native core: rendezvous over
// public Nostr/MQTT, MDP sessions (QUIC over hole-punched UDP), hardware capture/encode.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod auth;
mod bus;
mod core;
mod diag;
mod fm;
mod i18n;
mod input;
mod keymap;
mod keys;
mod mdp;
mod media;
mod perf;
mod platform;
mod quality;
mod rendezvous;
mod rt;
mod rv;
mod sprite;
mod store;
mod theme;
mod turncfg;
mod ui;
mod video;

/// "0.14.0 · build 20261020.1405" (build "dev" outside scripts/package-native.sh).
pub fn version() -> String {
    format!("{} · build {}", env!("CARGO_PKG_VERSION"), env!("MANYDESK_BUILD"))
}

fn main() -> eframe::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ManyDesk {}", version());
        return Ok(());
    }
    // Windows GUI apps have no console: MANYDESK_LOG=<file> captures stdout/stderr (tests).
    #[cfg(windows)]
    if let Ok(path) = std::env::var("MANYDESK_LOG") {
        use std::os::windows::io::IntoRawHandle;
        if let Ok(f) = std::fs::File::create(path) {
            let h = f.into_raw_handle();
            unsafe {
                use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
                SetStdHandle(STD_OUTPUT_HANDLE, h as _);
                SetStdHandle(STD_ERROR_HANDLE, h as _);
            }
        }
    }
    // Service control (Windows). These return before any window is created.
    #[cfg_attr(not(windows), allow(unused_variables))]
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    {
        if args.iter().any(|a| a == "--service") {
            platform::service::run();
            return Ok(());
        }
        if args.iter().any(|a| a == "--install-service") {
            return match platform::service::install() {
                Ok(()) => Ok(()),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
        }
        if args.iter().any(|a| a == "--uninstall-service") {
            let _ = platform::service::uninstall();
            return Ok(());
        }
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    rt::handle(); // start the runtime before anything spawns
    platform::keep_responsive();
    perf::start_watchdog();
    let rx = bus::init();
    // Some GL drivers (VMs, old GPUs, X11 without a compositor) have no surface with
    // alpha: then the app runs opaque and the host overlay is off (see the retry below).
    let transparent = std::env::var_os("MANYDESK_OPAQUE").is_none();
    ui::OVERLAY.store(transparent, std::sync::atomic::Ordering::Relaxed);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).ok();
    // macOS: the traffic lights sit inside our own title bar, like the web build's overlay title bar.
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ManyDesk")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([720.0, 480.0])
        // an alpha-capable GL surface, so the host overlay window can be transparent
        .with_transparent(transparent)
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    // Test hook: MANYDESK_WINPOS=x,y,w,h places the window (two instances side by side).
    if let Some(v) = std::env::var("MANYDESK_WINPOS").ok().map(|s| s.split(',').filter_map(|n| n.trim().parse::<f32>().ok()).collect::<Vec<_>>()).filter(|v| v.len() == 4) {
        viewport = viewport.with_position([v[0], v[1]]).with_inner_size([v[2], v[3]]);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let r = eframe::run_native("ManyDesk", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc, rx)))));
    if transparent && r.as_ref().is_err_and(|e| format!("{e:?}").contains("NoGlutinConfigs")) {
        // winit's event loop cannot be created twice in one process: start over opaque.
        if let Ok(exe) = std::env::current_exe() {
            // This process stays (the service watches it) but its windows would sit there
            // never answering a message — and a hung top-level window stalls everyone who
            // sends to all windows: display and full-screen changes froze other apps.
            #[cfg(windows)]
            drop_dead_windows();
            let status = std::process::Command::new(exe).args(std::env::args_os().skip(1)).env("MANYDESK_OPAQUE", "1").status();
            std::process::exit(status.map(|s| s.code().unwrap_or(0)).unwrap_or(1));
        }
    }
    r
}

/// Destroys the windows a failed event loop left on this thread, without running winit's
/// window procedure on its freed state.
#[cfg(windows)]
fn drop_dead_windows() {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, DestroyWindow, EnumThreadWindows, GetClassNameW, SetWindowLongPtrW, GWLP_WNDPROC};
    unsafe extern "system" fn each(h: HWND, out: LPARAM) -> BOOL {
        let mut class = [0u16; 64];
        let n = GetClassNameW(h, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        // the input method's own windows go with the thread's last window
        if !String::from_utf16_lossy(&class[..n]).contains("IME") {
            (*(out as *mut Vec<HWND>)).push(h);
        }
        1
    }
    let mut found: Vec<HWND> = Vec::new();
    unsafe {
        EnumThreadWindows(GetCurrentThreadId(), Some(each), &mut found as *mut Vec<HWND> as LPARAM);
        for h in found {
            SetWindowLongPtrW(h, GWLP_WNDPROC, DefWindowProcW as usize as isize);
            DestroyWindow(h);
        }
    }
}
