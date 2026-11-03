// Remote-control input on the host: viewer events (normalized 0..1 coordinates,
// KeyboardEvent.code names) are mapped onto the shared monitor and injected —
// macOS/Windows through the small native helpers (native/InputHelper.swift,
// native/win/inputhelper_windows.go, JSON lines on stdin), Linux/X11 through XTEST.
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::keymap;
use crate::media::capture::Screen;

#[derive(Clone, Copy)]
struct Area {
    // monitor origin and size in pixels; `scale` = pixels per point
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
}

impl Area {
    fn of(s: &Screen) -> Area {
        Area { x: s.x as f64, y: s.y as f64, w: s.width as f64, h: s.height as f64, scale: s.scale.max(1.0) }
    }
}

/// A display in desktop points: (x, y, width, height), for the overlay window over it.
pub fn overlay_rect(s: &Screen) -> (f32, f32, f32, f32) {
    let a = Area::of(s);
    // macOS: origin already in points; elsewhere everything is in pixels.
    let (x, y) = if cfg!(target_os = "macos") { (a.x, a.y) } else { (a.x / a.scale, a.y / a.scale) };
    (x as f32, y as f32, (a.w / a.scale) as f32, (a.h / a.scale) as f32)
}

pub struct Input {
    /// The helper, and the queue its writer thread feeds into its stdin: a helper that
    /// stops reading (stuck in SendInput on a secure desktop, say) must not stop the UI
    /// thread with it — a full queue drops input instead.
    child: Mutex<Option<(Child, SyncSender<String>)>>,
    /// what the helper last said about privacy-mode blocking (it answers each "block")
    blocked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "linux")]
    x11: Mutex<Option<crate::platform::xinput::XInput>>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            blocked: Default::default(),
            #[cfg(target_os = "linux")]
            x11: Mutex::new(None),
        }
    }

    fn helper_path() -> Option<std::path::PathBuf> {
        // Windows: the helper ships inside the exe (one-file portable app).
        #[cfg(windows)]
        {
            static HELPER: &[u8] = include_bytes!("../../build/bin-win/manydesk-input.exe");
            let dir = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("ManyDesk");
            let path = dir.join("manydesk-input.exe");
            if std::fs::metadata(&path).map(|m| m.len() != HELPER.len() as u64).unwrap_or(true) {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(&path, HELPER);
            }
            if path.exists() {
                return Some(path);
            }
        }
        let name = if cfg!(windows) { "manydesk-input.exe" } else { "manydesk-input" };
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        [dir.join(name), dir.join("bin").join(name), dir.join("../Resources/bin").join(name), dir.join("../../../build/bin").join(name)]
            .into_iter()
            .find(|p| p.exists())
    }

    pub fn start(&self) {
        #[cfg(mobile)]
        return; // Android: the accessibility service does it (android.rs); iOS: no control
        #[cfg(target_os = "linux")]
        {
            let mut x = self.x11.lock().unwrap();
            if x.is_none() {
                *x = crate::platform::xinput::XInput::new();
            }
            return;
        }
        #[allow(unreachable_code)]
        {
            let mut child = self.child.lock().unwrap();
            if child.is_some() {
                return;
            }
            let Some(path) = Self::helper_path() else {
                eprintln!("[input] helper missing");
                return;
            };
            let mut cmd = Command::new(path);
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            match cmd.spawn() {
                Ok(mut c) => {
                    if let Some(out) = c.stdout.take() {
                        let blocked = self.blocked.clone();
                        std::thread::spawn(move || {
                            use std::io::BufRead;
                            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                                if v["t"] == "block" {
                                    blocked.store(v["on"].as_bool().unwrap_or(false), std::sync::atomic::Ordering::Relaxed);
                                }
                            }
                        });
                    }
                    if let Some(mut stdin) = c.stdin.take() {
                        let (tx, rx) = sync_channel::<String>(512);
                        std::thread::Builder::new()
                            .name("input-writer".into())
                            .spawn(move || {
                                // ends when the queue is dropped, and the helper with its stdin
                                for line in rx {
                                    if writeln!(stdin, "{line}").is_err() {
                                        break;
                                    }
                                }
                            })
                            .ok();
                        *child = Some((c, tx));
                    }
                }
                Err(e) => eprintln!("[input] helper failed to start: {e}"),
            }
        }
    }

    pub fn stop(&self) {
        self.send(&json!({ "t": "release" }));
        self.blocked.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some((mut c, tx)) = self.child.lock().unwrap().take() {
            drop(tx);
            // the helper quits when its stdin closes; one that hangs is killed
            std::thread::spawn(move || {
                for _ in 0..30 {
                    if !matches!(c.try_wait(), Ok(None)) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let _ = c.kill();
                let _ = c.wait();
            });
        }
    }

    fn send(&self, cmd: &Value) {
        #[cfg(target_os = "android")]
        crate::android::inject(&cmd.to_string());
        // iOS lets no app control the phone
        #[cfg(target_os = "ios")]
        let _ = cmd;
        #[cfg(not(mobile))]
        {
            #[cfg(target_os = "linux")]
            if let Some(x) = self.x11.lock().unwrap().as_mut() {
                x.handle(cmd);
                return;
            }
            if let Some((_, tx)) = self.child.lock().unwrap().as_ref() {
                let _ = tx.try_send(cmd.to_string());
            }
        }
    }

    /// Privacy mode: only our own injected input reaches the apps (macOS: an event tap,
    /// Windows: BlockInput). False where it can't be done (Linux, or no helper).
    pub fn block(&self, on: bool) -> bool {
        if cfg!(target_os = "linux") {
            return !on;
        }
        if on {
            self.start();
        }
        let running = self.child.lock().unwrap().is_some();
        self.send(&json!({ "t": "block", "on": on }));
        running || !on
    }

    /// Whether the helper confirmed the block (it may lack the rights: Windows needs an
    /// elevated or SYSTEM helper, macOS the Accessibility permission).
    pub fn blocked(&self) -> bool {
        self.blocked.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Injects one viewer event on `screen` (the display that viewer is shown);
    /// returns the pointer position in that display's points.
    pub fn handle(&self, screen: &Screen, msg: &Value) -> Option<(f64, f64)> {
        let t = msg["t"].as_str()?;
        let a = Area::of(screen);
        let clamp01 = |v: &Value| v.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
        let point = || {
            let (nx, ny) = (clamp01(&msg["x"]), clamp01(&msg["y"]));
            // macOS: origin in points, size in pixels; CGEvent wants points.
            // Windows/Linux: everything in pixels.
            let (gx, gy) = if cfg!(target_os = "macos") {
                (a.x + nx * a.w / a.scale, a.y + ny * a.h / a.scale)
            } else {
                (a.x + nx * a.w, a.y + ny * a.h)
            };
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
                if cfg!(target_os = "android") {
                    // mapped in AccessibilityService terms on the Java side (InputService)
                    self.send(&json!({ "t": "key", "code": code, "d": d }));
                } else if cfg!(windows) {
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
            // translated keyboard mode: the characters themselves
            "text" => {
                let s: String = msg["s"].as_str().unwrap_or("").chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(4096).collect();
                if !s.is_empty() {
                    self.send(&json!({ "t": "text", "s": s }));
                }
                None
            }
            _ => None,
        }
    }
}
