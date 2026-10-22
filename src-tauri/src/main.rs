// ManyDesk — Tauri shell. Provides the same window.md API as the Electron main process
// (see src/renderer/tauri-bridge.js), so the renderer (UI + WebRTC) is shared.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod mdp;
mod media;
#[cfg(target_os = "linux")]
mod linux;
mod keymap;
mod rendezvous;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::menu::{Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use input::{Input, Who};
use rendezvous::Rendezvous;

struct HostState {
    active: bool,
    control: bool,
    clipboard: bool,
    awake: Option<Awake>,
}

// Keeps the display awake while someone is connected; released on drop.
struct Awake {
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
}

impl Awake {
    fn new() -> Self {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::*;
            unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) };
            Awake {}
        }
        #[cfg(not(windows))]
        {
            #[cfg(target_os = "macos")]
            let cmd = std::process::Command::new("caffeinate").args(["-d", "-i", "-w", &std::process::id().to_string()]).spawn();
            #[cfg(not(target_os = "macos"))]
            let cmd = std::process::Command::new("systemd-inhibit")
                .args(["--what=idle", "--who=ManyDesk", "--why=Remote session", "sleep", "infinity"])
                .spawn();
            Awake { child: cmd.ok() }
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Power::*;
            SetThreadExecutionState(ES_CONTINUOUS);
        }
        #[cfg(not(windows))]
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct Clip {
    host: AtomicBool,
    viewer: AtomicBool,
    last: Mutex<String>,
}

struct AppState {
    rv: Arc<Rendezvous>,
    input: Input,
    host: Mutex<HostState>,
    clip: Arc<Clip>,
    files: Mutex<HashMap<String, (PathBuf, File)>>,
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") { "darwin" } else if cfg!(windows) { "win32" } else { "linux" }
}

// ---- app info / language ----
#[derive(Serialize)]
struct AppInfo {
    name: String,
    platform: &'static str,
    version: String,
    languages: Vec<String>,
}

#[tauri::command]
fn app_info(app: AppHandle) -> AppInfo {
    let host = gethostname::gethostname().to_string_lossy().trim_end_matches(".local").to_string();
    AppInfo { name: host, platform: platform(), version: app.package_info().version.to_string(), languages: sys_locale::get_locales().collect() }
}

// ---- permissions (only macOS gates capture and input) ----
#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGPreflightScreenCaptureAccess() -> bool;
    }
}

#[tauri::command]
fn perm_status() -> Value {
    #[cfg(target_os = "macos")]
    let (screen, ax) = unsafe { (mac::CGPreflightScreenCaptureAccess(), mac::AXIsProcessTrusted()) };
    #[cfg(not(target_os = "macos"))]
    let (screen, ax) = (true, true);
    json!({ "screen": if screen { "granted" } else { "denied" }, "accessibility": ax })
}

#[tauri::command]
fn perm_open_settings(which: String) {
    #[cfg(target_os = "macos")]
    {
        let anchor = if which == "screen" { "Privacy_ScreenCapture" } else { "Privacy_Accessibility" };
        let _ = std::process::Command::new("open").arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}")).spawn();
    }
    let _ = which;
}

#[tauri::command]
fn perm_request_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        let trusted = unsafe { mac::AXIsProcessTrusted() };
        if !trusted {
            perm_open_settings("accessibility".into());
        }
        trusted
    }
    #[cfg(not(target_os = "macos"))]
    true
}

// ---- debug (MANYDESK_DEBUG=1): the page reports media state to stdout ----
#[tauri::command]
fn debug_enabled() -> bool {
    std::env::var_os("MANYDESK_DEBUG").is_some()
}

/// Test hook: MANYDESK_AUTOCONNECT=<id> connects on start (headless Linux tests).
#[tauri::command]
fn debug_autoconnect() -> Option<String> {
    std::env::var("MANYDESK_AUTOCONNECT").ok()
}

/// Test hook: MANYDESK_AUTOACCEPT=1 accepts incoming requests (headless host tests).
#[tauri::command]
fn debug_autoaccept() -> bool {
    std::env::var_os("MANYDESK_AUTOACCEPT").is_some()
}

#[tauri::command]
fn debug_log(msg: String) {
    println!("[page] {msg}");
}

// ---- host session ----
#[tauri::command]
fn host_session(app: AppHandle, st: State<AppState>, active: bool, control: bool, clipboard: bool) {
    let mut h = st.host.lock().unwrap();
    h.active = active;
    h.control = active && control;
    h.clipboard = active && clipboard;
    if h.control { st.input.start(&app) } else { st.input.stop(); st.input.close_overlay(&app); }
    if active && h.awake.is_none() {
        h.awake = Some(Awake::new());
    }
    if !active {
        h.awake = None;
    }
    st.clip.host.store(h.clipboard, Ordering::Relaxed);
}

#[tauri::command]
fn host_set_display(app: AppHandle, st: State<AppState>, width: f64, height: f64) {
    st.input.set_display(&app, width, height);
}

#[tauri::command]
fn input_event(app: AppHandle, st: State<AppState>, msg: Value, who: Option<Who>) {
    let (active, control) = {
        let h = st.host.lock().unwrap();
        (h.active, h.control)
    };
    if !(active && control) {
        if debug_enabled() {
            eprintln!("[input] dropped (active={active}, control={control})");
        }
        return;
    }
    if debug_enabled() {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        if n % 25 == 0 {
            eprintln!("[input] event #{n}: {msg}");
        }
    }
    let point = st.input.handle(&app, &msg);
    let kind = msg["t"].as_str().unwrap_or("");
    if let Some(w) = who {
        if ["move", "down", "up", "scroll", "key"].contains(&kind) {
            let mut data = json!({ "id": w.id, "name": w.name.chars().take(40).collect::<String>(), "tag": w.tag, "color": w.color, "acts": w.acts, "kind": kind });
            if let Some((x, y)) = point {
                data["x"] = json!(x);
                data["y"] = json!(y);
            }
            st.input.overlay_send(&app, "cursor", data);
        }
    }
}

#[tauri::command]
fn input_forget(app: AppHandle, st: State<AppState>, id: i64) {
    if app.get_webview_window("overlay").is_some() {
        st.input.overlay_send(&app, "forget", json!(id));
    }
}

#[tauri::command]
fn overlay_ready(app: AppHandle, st: State<AppState>) {
    st.input.overlay_ready(&app);
}

// ---- clipboard ----
#[tauri::command]
fn clipboard_watch(st: State<AppState>, on: bool) {
    st.clip.viewer.store(on, Ordering::Relaxed);
}

#[tauri::command]
fn clipboard_set(st: State<AppState>, text: String) {
    if text.len() > 1_000_000 {
        return;
    }
    // Remember it so the watcher does not report our own write as a change.
    *st.clip.last.lock().unwrap() = text.clone();
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text);
    }
}

fn start_clipboard_watcher(app: AppHandle, clip: Arc<Clip>) {
    std::thread::spawn(move || {
        let mut cb = arboard::Clipboard::new().ok();
        loop {
            std::thread::sleep(Duration::from_millis(700));
            if !(clip.host.load(Ordering::Relaxed) || clip.viewer.load(Ordering::Relaxed)) {
                continue;
            }
            let Some(c) = cb.as_mut() else { cb = arboard::Clipboard::new().ok(); continue };
            if let Ok(text) = c.get_text() {
                let mut last = clip.last.lock().unwrap();
                if *last != text {
                    *last = text.clone();
                    if !text.is_empty() {
                        let _ = app.emit_to("main", "clipboard:changed", text);
                    }
                }
            }
        }
    });
}

// ---- menu (capture mode drops the Edit menu so Cmd+C/V reach the remote side) ----
fn build_menu(app: &AppHandle, capture: bool) -> tauri::Result<Menu<tauri::Wry>> {
    if !capture {
        return Menu::default(app);
    }
    let app_menu = Submenu::with_items(app, "ManyDesk", true, &[&PredefinedMenuItem::quit(app, None)?])?;
    let window = Submenu::with_items(app, "Window", true, &[&PredefinedMenuItem::minimize(app, None)?, &PredefinedMenuItem::fullscreen(app, None)?])?;
    Menu::with_items(app, &[&app_menu, &window])
}

#[tauri::command]
fn menu_capture(app: AppHandle, on: bool) {
    if cfg!(target_os = "macos") {
        if let Ok(m) = build_menu(&app, on) {
            let _ = app.set_menu(m);
        }
    }
}

#[tauri::command]
fn set_fullscreen(app: AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_fullscreen(on);
    }
}

// ---- rendezvous ----
#[tauri::command]
fn rv_subscribe(st: State<AppState>, topic: String, lookback: Option<i64>) -> bool {
    if !is_topic(&topic) {
        return false;
    }
    st.rv.subscribe(&topic, lookback == Some(0));
    true
}

#[tauri::command]
fn rv_unsubscribe(st: State<AppState>, topic: String) {
    if is_topic(&topic) {
        st.rv.unsubscribe(&topic);
    }
}

#[tauri::command]
async fn rv_publish(st: State<'_, AppState>, topic: String, content: String) -> Result<rendezvous::PublishResult, String> {
    if !is_topic(&topic) || content.len() > 60_000 {
        return Err("invalid".into());
    }
    let rv = st.rv.clone();
    Ok(rv.publish(&topic, &content).await)
}

#[tauri::command]
fn rv_status(st: State<AppState>) -> rendezvous::Status {
    st.rv.status()
}

fn is_topic(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ---- Cloudflare Realtime TURN (1,000 GB/month free): short-lived ICE credentials ----
#[tauri::command]
async fn turn_cloudflare(key_id: String, token: String) -> Value {
    let valid = (8..=128).contains(&key_id.len()) && key_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid || token.trim().is_empty() {
        return json!({ "error": "cf.badInput" });
    }
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(c) => c,
        Err(e) => return json!({ "error": "cf.net", "detail": e.to_string() }),
    };
    let url = format!("https://rtc.live.cloudflare.com/v1/turn/keys/{key_id}/credentials/generate-ice-servers");
    let res = match client.post(url).bearer_auth(token.trim()).json(&json!({ "ttl": 86400 })).send().await {
        Ok(r) => r,
        Err(e) => return json!({ "error": "cf.net", "detail": e.to_string() }),
    };
    let status = res.status().as_u16();
    if !res.status().is_success() {
        let code = match status { 401 | 403 => "cf.auth", 404 => "cf.notFound", _ => "cf.http" };
        return json!({ "error": code, "status": status });
    }
    let data: Value = res.json().await.unwrap_or(Value::Null);
    let turn = data["iceServers"].as_array().and_then(|a| a.iter().find(|s| s["username"].is_string() && s["credential"].is_string()).cloned());
    let Some(turn) = turn else { return json!({ "error": "cf.noTurn" }) };
    let urls: Vec<String> = match &turn["urls"] {
        Value::Array(a) => a.iter().filter_map(|u| u.as_str()).filter(|u| u.starts_with("turn")).map(String::from).collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    };
    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) + 86_400_000;
    json!({ "urls": urls, "username": turn["username"], "credential": turn["credential"], "expires": expires })
}

// ---- incoming files → Downloads/ManyDesk ----
fn downloads_dir(app: &AppHandle) -> PathBuf {
    let d = app.path().download_dir().unwrap_or_else(|_| std::env::temp_dir()).join("ManyDesk");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe: String = Path::new(name).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        .chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).take(200).collect();
    let safe = if safe.is_empty() { "file".to_string() } else { safe };
    let p = Path::new(&safe);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut out = dir.join(&safe);
    let mut i = 1;
    while out.exists() {
        out = dir.join(format!("{stem} ({i}){ext}"));
        i += 1;
    }
    out
}

#[tauri::command]
fn file_begin(app: AppHandle, st: State<AppState>, id: String, name: String) -> Result<String, String> {
    let path = unique_path(&downloads_dir(&app), &name);
    let f = File::create(&path).map_err(|e| e.to_string())?;
    st.files.lock().unwrap().insert(id, (path.clone(), f));
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn file_chunk(st: State<AppState>, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let id = request.headers().get("x-id").and_then(|v| v.to_str().ok()).ok_or("no id")?.to_string();
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else { return Err("expected bytes".into()) };
    let mut files = st.files.lock().unwrap();
    let (_, f) = files.get_mut(&id).ok_or("unknown file")?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

#[tauri::command]
fn file_end(st: State<AppState>, id: String, ok: bool) -> Option<String> {
    let (path, f) = st.files.lock().unwrap().remove(&id)?;
    drop(f);
    if ok {
        Some(path.to_string_lossy().to_string())
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

#[tauri::command]
fn file_reveal(app: AppHandle, path: String) {
    let p = PathBuf::from(&path);
    if !p.starts_with(downloads_dir(&app)) {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(&p).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(&p)).spawn();
}

macro_rules! handlers {
    ($($extra:path),*) => {
        tauri::generate_handler![
            debug_enabled, debug_autoconnect, debug_autoaccept, debug_log, app_info, perm_status, perm_open_settings, perm_request_accessibility,
            host_session, host_set_display, input_event, input_forget, overlay_ready,
            clipboard_watch, clipboard_set, menu_capture, set_fullscreen,
            rv_subscribe, rv_unsubscribe, rv_publish, rv_status, turn_cloudflare,
            file_begin, file_chunk, file_end, file_reveal,
            media::screens_list, mdp::mdp_prepare, mdp::mdp_add_video, mdp::mdp_connect, mdp::mdp_send, mdp::mdp_send_bin,
            mdp::mdp_video_params, mdp::mdp_keyframe, mdp::mdp_audio, mdp::mdp_stats, mdp::mdp_close $(, $extra)*
        ]
    };
}

// Linux adds the native WebRTC commands (src/linux/rtc.rs).
#[cfg(target_os = "linux")]
macro_rules! all_handlers {
    () => {
        handlers!(
            linux::rtc::npc_create, linux::rtc::npc_add_video, linux::rtc::npc_add_audio, linux::rtc::npc_offer,
            linux::rtc::npc_answer, linux::rtc::npc_set_local, linux::rtc::npc_set_remote, linux::rtc::npc_local,
            linux::rtc::npc_add_candidate, linux::rtc::npc_dc_create, linux::rtc::npc_dc_send, linux::rtc::npc_dc_send_bin,
            linux::rtc::npc_dc_close, linux::rtc::npc_video_params, linux::rtc::npc_pli, linux::rtc::npc_stats,
            linux::rtc::npc_close
        )
    };
}
#[cfg(not(target_os = "linux"))]
macro_rules! all_handlers {
    () => {
        handlers!()
    };
}

fn main() {
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
    let builder = tauri::Builder::default().manage(mdp::Mdp::default());
    #[cfg(target_os = "linux")]
    let builder = builder.manage(linux::rtc::NativeRtc::default());
    builder
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let rv = Rendezvous::new(handle.clone());
            rv.start();
            let clip = Arc::new(Clip { host: AtomicBool::new(false), viewer: AtomicBool::new(false), last: Mutex::new(String::new()) });
            start_clipboard_watcher(handle.clone(), clip.clone());
            app.manage(AppState {
                rv,
                input: Input::new(),
                host: Mutex::new(HostState { active: false, control: false, clipboard: false, awake: None }),
                clip,
                files: Mutex::new(HashMap::new()),
            });
            // Tell the page when the window enters/leaves full screen (OS button or our call).
            if let Some(win) = app.get_webview_window("main") {
                let w = win.clone();
                let last = Arc::new(AtomicBool::new(false));
                win.on_window_event(move |e| {
                    if matches!(e, WindowEvent::Resized(_)) {
                        let fs = w.is_fullscreen().unwrap_or(false);
                        if last.swap(fs, Ordering::Relaxed) != fs {
                            let _ = w.emit("win:fullscreen", fs);
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(all_handlers!())
        .build(tauri::generate_context!())
        .expect("failed to build ManyDesk")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(st) = app.try_state::<AppState>() {
                    st.input.stop();
                }
            }
        });
}
