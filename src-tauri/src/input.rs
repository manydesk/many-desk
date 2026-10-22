// Remote-control input: viewer events (normalized 0..1 coordinates, KeyboardEvent.code)
// are mapped to the shared monitor and written as JSON lines to the native helper
// (native/InputHelper.swift on macOS, native/win/inputhelper_windows.go on Windows),
// the same helpers the Electron build uses. Also owns the host-only pointer overlay.
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

use crate::keymap;

#[derive(Clone, Copy)]
struct Area {
    // physical pixels (Windows SetCursorPos) and scale (macOS CGEvent uses points)
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
}

#[derive(Deserialize, Clone)]
pub struct Who {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub acts: Value,
}

pub struct Input {
    child: Mutex<Option<(Child, ChildStdin)>>,
    area: Mutex<Option<Area>>,
    overlay_ready: Mutex<bool>,
    overlay_queue: Mutex<Vec<(String, Value)>>,
    // Linux: XTEST in-process instead of a helper.
    #[cfg(target_os = "linux")]
    x11: Mutex<Option<crate::linux::xinput::XInput>>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            area: Mutex::new(None),
            overlay_ready: Mutex::new(false),
            overlay_queue: Mutex::new(vec![]),
            #[cfg(target_os = "linux")]
            x11: Mutex::new(None),
        }
    }

    fn helper_path(app: &AppHandle) -> Option<std::path::PathBuf> {
        let name = if cfg!(windows) { "manydesk-input.exe" } else { "manydesk-input" };
        let mut candidates = vec![];
        if let Ok(res) = app.path().resource_dir() {
            candidates.push(res.join("bin").join(name));
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("build");
        candidates.push(root.join(if cfg!(windows) { "bin-win" } else { "bin" }).join(name));
        candidates.into_iter().find(|p| p.exists())
    }

    pub fn start(&self, app: &AppHandle) {
        #[cfg(target_os = "linux")]
        {
            let mut x = self.x11.lock().unwrap();
            if x.is_none() {
                *x = crate::linux::xinput::XInput::new();
                eprintln!("[input] XTEST {}", if x.is_some() { "ready" } else { "unavailable" });
            }
            let _ = app;
            return;
        }
        #[allow(unreachable_code)]
        let mut child = self.child.lock().unwrap();
        if child.is_some() {
            return;
        }
        let Some(path) = Self::helper_path(app) else {
            eprintln!("[input] helper missing — run npm run build:helper");
            return;
        };
        eprintln!("[input] helper {}", path.display());
        let mut cmd = Command::new(path);
        // A GUI app may have no stderr to inherit (Windows), so only pass it on when debugging.
        let debug = std::env::var_os("MANYDESK_DEBUG").is_some();
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(if debug { Stdio::inherit() } else { Stdio::null() });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.spawn() {
            Ok(mut c) => {
                if let Some(stdin) = c.stdin.take() {
                    *child = Some((c, stdin));
                }
            }
            Err(e) => eprintln!("[input] helper failed to start: {e}"),
        }
    }

    pub fn stop(&self) {
        if std::env::var_os("MANYDESK_DEBUG").is_some() && self.child.lock().unwrap().is_some() {
            eprintln!("[input] helper stopped");
        }
        self.send(&json!({ "t": "release" }));
        if let Some((mut c, stdin)) = self.child.lock().unwrap().take() {
            drop(stdin);
            let _ = c.wait();
        }
    }

    fn send(&self, cmd: &Value) {
        // A helper that died (e.g. crashed) is noticed here and restarted by the next start().
        if let Some((c, _)) = self.child.lock().unwrap().as_mut() {
            if let Ok(Some(st)) = c.try_wait() {
                eprintln!("[input] helper exited: {st}");
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(x) = self.x11.lock().unwrap().as_mut() {
            x.handle(cmd);
            return;
        }
        if let Some((_, stdin)) = self.child.lock().unwrap().as_mut() {
            let _ = writeln!(stdin, "{cmd}");
        }
    }

    // The captured surface's pixel size picks the monitor (system picker = user's choice).
    pub fn set_display(&self, app: &AppHandle, width: f64, height: f64) {
        let monitors = app.available_monitors().unwrap_or_default();
        let pick = monitors
            .iter()
            .find(|m| {
                let s = m.size();
                let sc = m.scale_factor();
                (s.width as f64 - width).abs() < 4.0 && (s.height as f64 - height).abs() < 4.0
                    || ((s.width as f64 / sc) - width).abs() < 4.0 && ((s.height as f64 / sc) - height).abs() < 4.0
            })
            .cloned()
            .or_else(|| app.primary_monitor().ok().flatten());
        if let Some(m) = pick {
            let (p, s) = (m.position(), m.size());
            *self.area.lock().unwrap() = Some(Area { x: p.x as f64, y: p.y as f64, w: s.width as f64, h: s.height as f64, scale: m.scale_factor() });
            if let Some(o) = app.get_webview_window("overlay") {
                let _ = o.set_position(PhysicalPosition::new(p.x, p.y));
                let _ = o.set_size(PhysicalSize::new(s.width, s.height));
            }
        }
    }

    fn area(&self, app: &AppHandle) -> Option<Area> {
        if self.area.lock().unwrap().is_none() {
            if let Some(m) = app.primary_monitor().ok().flatten() {
                let (p, s) = (m.position(), m.size());
                *self.area.lock().unwrap() = Some(Area { x: p.x as f64, y: p.y as f64, w: s.width as f64, h: s.height as f64, scale: m.scale_factor() });
            }
        }
        *self.area.lock().unwrap()
    }

    /// Injects one viewer event; returns the pointer position in overlay (logical) pixels.
    pub fn handle(&self, app: &AppHandle, msg: &Value) -> Option<(f64, f64)> {
        let t = msg["t"].as_str()?;
        let a = self.area(app)?;
        let clamp01 = |v: &Value| v.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        let point = || {
            let (nx, ny) = (clamp01(&msg["x"]), clamp01(&msg["y"]));
            let (px, py) = (a.x + nx * a.w, a.y + ny * a.h);
            // macOS CGEvent wants points; Windows SetCursorPos wants physical pixels.
            let (gx, gy) = if cfg!(target_os = "macos") { (px / a.scale, py / a.scale) } else { (px, py) };
            (gx, gy, nx * a.w / a.scale, ny * a.h / a.scale)
        };
        match t {
            "move" => {
                let (gx, gy, lx, ly) = point();
                self.send(&json!({ "t": "move", "x": gx, "y": gy }));
                Some((lx, ly))
            }
            "down" | "up" => {
                let (gx, gy, lx, ly) = point();
                let b = msg["b"].as_i64().filter(|b| (0..=2).contains(b)).unwrap_or(0);
                self.send(&json!({ "t": t, "b": b, "c": msg["c"].as_i64().unwrap_or(1), "x": gx, "y": gy }));
                Some((lx, ly))
            }
            "scroll" => {
                let c = |v: &Value| v.as_f64().unwrap_or(0.0).clamp(-2000.0, 2000.0);
                self.send(&json!({ "t": "scroll", "dx": c(&msg["dx"]), "dy": c(&msg["dy"]) }));
                None
            }
            "key" => {
                let code = msg["code"].as_str().unwrap_or("");
                let d = msg["d"].as_bool().unwrap_or(false);
                if cfg!(windows) {
                    if let Some((k, e)) = keymap::win(code) {
                        self.send(&json!({ "t": "key", "k": k, "e": e, "d": d }));
                    }
                } else if cfg!(target_os = "linux") {
                    if let Some(k) = keymap::linux(code) {
                        self.send(&json!({ "t": "key", "k": k, "d": d }));
                    }
                } else if let Some(k) = keymap::mac(code) {
                    self.send(&json!({ "t": "key", "k": k, "d": d }));
                }
                None
            }
            "release" => {
                self.send(&json!({ "t": "release" }));
                None
            }
            _ => None,
        }
    }

    // ---- host-only pointer overlay ----
    // Transparent, click-through, always on top and excluded from screen capture
    // (content protection), so only the host sees who is doing what.
    fn ensure_overlay(&self, app: &AppHandle) {
        if app.get_webview_window("overlay").is_some() {
            return;
        }
        *self.overlay_ready.lock().unwrap() = false;
        let Some(a) = self.area(app) else { return };
        let built = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
            .title("ManyDesk overlay")
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .resizable(false)
            .visible_on_all_workspaces(true)
            .content_protected(true)
            .position(a.x / a.scale, a.y / a.scale)
            .inner_size(a.w / a.scale, a.h / a.scale)
            .build();
        if let Ok(w) = built {
            let _ = w.set_ignore_cursor_events(true);
        }
    }

    pub fn overlay_ready(&self, app: &AppHandle) {
        *self.overlay_ready.lock().unwrap() = true;
        for (ch, data) in self.overlay_queue.lock().unwrap().drain(..) {
            let _ = app.emit_to("overlay", &ch, data);
        }
    }

    pub fn overlay_send(&self, app: &AppHandle, channel: &str, data: Value) {
        self.ensure_overlay(app);
        if *self.overlay_ready.lock().unwrap() {
            let _ = app.emit_to("overlay", channel, data);
        } else {
            self.overlay_queue.lock().unwrap().push((channel.to_string(), data));
        }
    }

    pub fn close_overlay(&self, app: &AppHandle) {
        if let Some(w) = app.get_webview_window("overlay") {
            let _ = w.destroy();
        }
        *self.overlay_ready.lock().unwrap() = false;
        self.overlay_queue.lock().unwrap().clear();
    }
}
