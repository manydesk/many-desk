// Clipboard beyond text: pictures and copied files go to the other side's clipboard too.
// They travel over the file stream as a small batch whose id starts with "clip-img-",
// "clip-files-" or "clip-paste-"; the receiver keeps them in a temp folder and puts that on
// its clipboard (Ctrl+V / Cmd+V then pastes them there). Text stays on the control stream
// ("clip").
//
// Files copied on the viewer's side wait for a paste, as in AnyDesk: Ctrl+V / Cmd+V in the
// session sends them ("clip-paste-", shown with the file manager's copies), and once they
// are in, the host puts them on its clipboard and pastes them itself. Files copied on the
// host go over to the viewer right away (up to MAX_FILES_BYTES): the viewer pastes them
// outside ManyDesk, where nothing tells us when.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// Files on the clipboard are sent along only up to this much.
const MAX_FILES_BYTES: u64 = 256 << 20;
const MAX_FILES: usize = 2000;
/// Pasted into a session (the copy shows, and can be stopped, in the file manager).
const MAX_PASTE_BYTES: u64 = 4 << 30;
const MAX_PASTE_FILES: usize = 20000;
/// A picture bigger than this (RGBA) is not sent.
#[cfg_attr(mobile, allow(dead_code))] // pictures and files: desktops only
const MAX_IMAGE_BYTES: usize = 64 << 20;

/// Set by the core: some live session shares the clipboard (pictures and files are only
/// looked at then — reading a big picture every poll is not free).
pub static CLIP_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Until this time (ms since 1970) clipboard changes are ours (set from the other side)
/// and are not sent back — only where the clipboard has no change count (Linux, pictures
/// and files; text is told apart by OURS.text).
#[cfg_attr(any(mobile, target_os = "macos", windows), allow(dead_code))]
static QUIET_UNTIL: AtomicU64 = AtomicU64::new(0);

/// What this side put on the clipboard itself, from the other side: the watcher does not
/// send it back. `seq`: the clipboard's change count right after (macOS, Windows).
struct Ours {
    text: Option<String>,
    seq: Option<u64>,
}

static OURS: Mutex<Ours> = Mutex::new(Ours { text: None, seq: None });

#[cfg_attr(any(mobile, target_os = "macos", windows), allow(dead_code))]
fn quiet(ms: u64) {
    QUIET_UNTIL.store(rv::now_ms() as u64 + ms, Ordering::SeqCst);
}

fn is_quiet() -> bool {
    (rv::now_ms() as u64) < QUIET_UNTIL.load(Ordering::SeqCst)
}

/// The clipboard's change count: it is read only after a change. Polling the contents
/// themselves keeps the clipboard open on Windows, where another app's copy or paste then
/// fails now and then, and on macOS reading it counts as a paste.
#[cfg(target_os = "macos")]
fn change_count() -> Option<u64> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    objc2::rc::autoreleasepool(|_| {
        let class = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut AnyObject = unsafe { msg_send![class, generalPasteboard] };
        if pb.is_null() {
            return None;
        }
        let n: isize = unsafe { msg_send![pb, changeCount] };
        Some(n as u64)
    })
}

#[cfg(windows)]
fn change_count() -> Option<u64> {
    // 0: no access to this desktop's clipboard (then the contents are polled)
    let n = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
    (n != 0).then_some(n as u64)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn change_count() -> Option<u64> {
    None
}

#[cfg(mobile)]
pub fn set_text(text: &str) {
    let mut ours = OURS.lock().unwrap();
    crate::phone::set_clipboard_text(text);
    ours.text = Some(text.to_string());
}

#[cfg(not(mobile))]
pub fn set_text(text: &str) {
    let mut ours = OURS.lock().unwrap();
    if let Ok(mut c) = arboard::Clipboard::new() {
        if c.set_text(text.to_string()).is_ok() {
            *ours = Ours { text: Some(text.to_string()), seq: change_count() };
        }
    }
}

/// Where received clipboard batches live (the newest few are kept for pasting).
fn clip_root() -> PathBuf {
    std::env::temp_dir().join("manydesk-clip")
}

pub fn clip_dir(batch: &str) -> Option<PathBuf> {
    let name: String = batch.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    if name.len() < 10 {
        return None;
    }
    let dir = clip_root().join(name);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Keeps the three newest received batches, removes the rest.
#[cfg_attr(mobile, allow(dead_code))] // pictures and files: desktops only
fn prune(keep: &Path) {
    let Ok(rd) = std::fs::read_dir(clip_root()) else { return };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = rd.filter_map(|e| e.ok()).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).filter(|(_, p)| p != keep).collect();
    dirs.sort();
    let n = dirs.len().saturating_sub(2);
    for (_, p) in dirs.into_iter().take(n) {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// Phones: text only (android.rs: ClipboardManager, ios.rs: UIPasteboard).
#[cfg(mobile)]
pub fn start_watch() {
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(|| {
            let mut last = crate::phone::clipboard_text().unwrap_or_default();
            loop {
                std::thread::sleep(Duration::from_millis(700));
                let Some(t) = crate::phone::clipboard_text() else { continue };
                if t != last {
                    last = t.clone();
                    if OURS.lock().unwrap().text.as_ref() != Some(&t) {
                        bus::emit("clip", json!({ "text": t }));
                    }
                }
            }
        })
        .ok();
}

/// Watches the clipboard (no portable change notification): its change count where there
/// is one, else the contents, and reports changes.
#[cfg(not(mobile))]
pub fn start_watch() {
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(|| {
            let Ok(mut c) = arboard::Clipboard::new() else { return };
            let mut seen = change_count();
            let mut last_text = c.get_text().unwrap_or_default();
            let mut last_files: Vec<PathBuf> = c.get().file_list().unwrap_or_default();
            let mut last_img: Option<u64> = None;
            let mut tick = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                tick += 1;
                let active = CLIP_ACTIVE.load(Ordering::Relaxed);
                let now = change_count();
                if now.is_some() && now == seen {
                    continue;
                }
                seen = now;
                let (ours_text, echo) = {
                    let ours = OURS.lock().unwrap();
                    (ours.text.clone(), now.is_some() && ours.seq == now)
                };
                let quiet = echo || is_quiet();
                // files first: a copied file also puts its name on the clipboard as text
                let files = if active { c.get().file_list().unwrap_or_default() } else { Vec::new() };
                if !files.is_empty() {
                    if files != last_files {
                        last_files = files.clone();
                        last_img = None;
                        if !quiet {
                            bus::emit("clip:files", json!({ "paths": files }));
                        }
                    }
                    last_text = c.get_text().unwrap_or_default();
                    continue;
                }
                last_files.clear();
                if let Ok(t) = c.get_text() {
                    if t != last_text {
                        last_text = t.clone();
                        last_img = None;
                        if !echo && ours_text.as_ref() != Some(&t) {
                            bus::emit("clip", json!({ "text": t }));
                        }
                    }
                    continue;
                }
                // a picture: after a change, or (no change count) every ~1.5 s
                if !active || (now.is_none() && tick % 3 != 0) {
                    continue;
                }
                let Ok(img) = c.get_image() else { continue };
                let sig = signature(img.width, img.height, &img.bytes);
                if last_img == Some(sig) {
                    continue;
                }
                last_img = Some(sig);
                if quiet || img.bytes.len() > MAX_IMAGE_BYTES {
                    continue;
                }
                let path = std::env::temp_dir().join(format!("manydesk-clip-out-{}.png", std::process::id()));
                let ok = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()).is_some_and(|i| i.save(&path).is_ok());
                if ok {
                    bus::emit("clip:image", json!({ "path": path }));
                }
            }
        })
        .ok();
}

/// A cheap fingerprint: size and a sample of the pixels.
#[cfg_attr(mobile, allow(dead_code))] // pictures and files: desktops only
fn signature(w: usize, h: usize, px: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (w, h, px.len()).hash(&mut hs);
    let step = (px.len() / 4096).max(1);
    px.iter().step_by(step).for_each(|b| b.hash(&mut hs));
    hs.finish()
}

impl Core {
    /// Sessions the clipboard goes to: hosts that allow it, and the session in front (not
    /// files: those wait for a paste there, see `paste_files`).
    pub(super) fn clip_targets(&self, files: bool) -> Vec<Option<u32>> {
        let mut out: Vec<Option<u32>> = self.hosts.iter().filter(|h| h.state == HostState::Live && h.perms.clipboard && (!files || h.perms.files)).map(|h| h.sid).collect();
        if let Some(t) = self.active.and_then(|k| self.tabs.iter().find(|t| t.key == k)) {
            if t.state == TabState::Live && t.remote.clipboard && !files {
                out.push(t.sid);
            }
        }
        out
    }

    pub(super) fn update_clip_active(&self) {
        CLIP_ACTIVE.store(!self.clip_targets(false).is_empty(), Ordering::Relaxed);
    }

    /// The local clipboard now holds a picture (saved as PNG at `path`) or files.
    pub(super) fn on_local_clip(&mut self, name: &str, v: &Value) {
        let files = name == "clip:files";
        let paths: Vec<PathBuf> = if files {
            v["paths"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(PathBuf::from)).collect()).unwrap_or_default()
        } else {
            v["path"].as_str().map(PathBuf::from).into_iter().collect()
        };
        if paths.is_empty() {
            return;
        }
        let targets: Vec<Arc<Session>> = self.clip_targets(files).into_iter().filter_map(|sid| self.session(sid)).collect();
        if targets.is_empty() {
            return;
        }
        let kind = if files { "files" } else { "img" };
        crate::rt::spawn(async move {
            if files {
                let p = paths.clone();
                let (bytes, count) = tokio::task::spawn_blocking(move || size_of(&p)).await.unwrap_or((u64::MAX, 0));
                if bytes > MAX_FILES_BYTES || count > MAX_FILES {
                    bus::emit("toast", json!({ "text": t("clip.tooBig"), "error": true }));
                    return;
                }
            }
            for s in targets {
                let batch = format!("clip-{kind}-{}", &rv::random_topic()[..16]);
                let _ = fm::send_batch(s, paths.clone(), batch, None, Arc::new(AtomicBool::new(false)), |_, _| {}).await;
            }
        });
    }

    /// Cmd/Ctrl+V into session `key` while this side's clipboard holds files: sends them,
    /// and the host pastes them when they are in. False: no files here (or the session
    /// takes none), the key goes over as usual.
    pub fn paste_files(&mut self, key: u32) -> bool {
        let Some(tab) = self.tabs.iter().find(|t| t.key == key) else { return false };
        if tab.state != TabState::Live || !tab.remote.clipboard || !tab.remote.files {
            return false;
        }
        let paths = local_files();
        if paths.is_empty() {
            return false;
        }
        let sid = tab.sid;
        let Some(s) = self.session(sid) else { return false };
        let shown: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let name = fm::summary(&shown, |n| tf("fm.items", &[("n", &n.to_string())]));
        let batch = format!("clip-paste-{}", &rv::random_topic()[..16]);
        let cancel = Arc::new(AtomicBool::new(false));
        let tab = self.tab(key).unwrap();
        // dest: no folder of the file manager, so finishing it refreshes none
        tab.fm.transfers.push(fm::Transfer { id: batch.clone(), name: name.clone(), upload: true, dest: "\0clip".into(), ..Default::default() });
        tab.fm.cancels.insert(batch.clone(), cancel.clone());
        let sid = sid.unwrap_or(0);
        crate::rt::spawn(async move {
            let p = paths.clone();
            let (bytes, count) = tokio::task::spawn_blocking(move || size_of(&p)).await.unwrap_or((u64::MAX, 0));
            let r = if bytes > MAX_PASTE_BYTES || count > MAX_PASTE_FILES {
                Err(t("clip.tooBig"))
            } else {
                bus::emit("toast", json!({ "text": tf("clip.pasting", &[("f", &name)]) }));
                let id = batch.clone();
                fm::send_batch(s, paths, batch.clone(), None, cancel, |done, total| {
                    bus::emit("fm:progress", json!({ "sid": sid, "id": id, "done": done, "total": total }));
                })
                .await
            };
            if let Err(e) = &r {
                bus::emit("toast", json!({ "text": e, "error": true }));
            }
            bus::emit("fm:done", json!({ "sid": sid, "id": batch, "x": matches!(r, Ok(false)), "e": r.err().unwrap_or_default() }));
        });
        true
    }

    /// Files pasted from the viewer are on this computer's clipboard now: Cmd/Ctrl+V here
    /// (the viewer's own keys went up long ago).
    pub(super) fn clip_paste_now(&mut self, sid: u32) {
        let Some(h) = self.hosts.iter().find(|h| h.sid == Some(sid) && h.state == HostState::Live && h.perms.control) else { return };
        let Some(screen) = h.screen.clone() else { return };
        let m = if cfg!(target_os = "macos") { "MetaLeft" } else { "ControlLeft" };
        for (code, d) in [(m, true), ("KeyV", true), ("KeyV", false), (m, false)] {
            let msg = json!({ "t": "key", "code": code, "d": d });
            if std::env::var_os("MANYDESK_TESTNOINJECT").is_some() {
                eprintln!("[clip] paste {msg}");
                continue;
            }
            self.input.handle(&screen, &msg);
        }
    }

    /// Whether this side takes a clipboard batch from session `sid`.
    pub(super) fn clip_allowed(&self, sid: u32, batch: &str) -> bool {
        let files = batch.starts_with("clip-files-") || batch.starts_with("clip-paste-");
        if let Some(t) = self.tabs.iter().find(|t| t.sid == Some(sid)) {
            return t.remote.clipboard && (!files || t.remote.files);
        }
        self.hosts.iter().any(|h| h.sid == Some(sid) && h.state == HostState::Live && h.perms.clipboard && (!files || h.perms.files))
    }

    /// A clipboard batch arrived complete: put it on this computer's clipboard (off the UI
    /// thread — decoding a big picture takes a while).
    pub(super) fn clip_received(&mut self, sid: u32, batch: &str) {
        let batch = batch.to_string();
        std::thread::spawn(move || {
            if put_received(&batch) && batch.starts_with("clip-paste-") {
                bus::emit("clip:paste", json!({ "sid": sid }));
            }
        });
    }
}

/// Bytes and entries under `paths` (folders recursively).
fn size_of(paths: &[PathBuf]) -> (u64, usize) {
    fm::walk(paths).iter().fold((0u64, 0usize), |(b, n), (path, _, d)| (b + if *d { 0 } else { std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) }, n + 1))
}

/// The files on this computer's clipboard (read only when pasting).
#[cfg(mobile)]
fn local_files() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(not(mobile))]
fn local_files() -> Vec<PathBuf> {
    // Test hook: MANYDESK_TESTPASTEFILES=<path>[:<path>…] stands in for copied files.
    if let Some(v) = std::env::var_os("MANYDESK_TESTPASTEFILES") {
        return std::env::split_paths(&v).collect();
    }
    arboard::Clipboard::new().ok().and_then(|mut c| c.get().file_list().ok()).unwrap_or_default()
}

#[cfg(mobile)]
fn put_received(_batch: &str) -> bool {
    false
}

#[cfg(not(mobile))]
fn put_received(batch: &str) -> bool {
    let Some(dir) = clip_dir(batch) else { return false };
    prune(&dir);
    let Ok(mut c) = arboard::Clipboard::new() else { return false };
    // decoded before taking OURS: the watcher waits on it
    let img = if batch.starts_with("clip-img-") {
        let Some(png) = std::fs::read_dir(&dir).ok().and_then(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).next()) else { return false };
        let Ok(img) = image::open(&png) else { return false };
        Some(img.to_rgba8())
    } else {
        None
    };
    let mut ours = OURS.lock().unwrap();
    let set = if let Some(rgba) = img {
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        c.set_image(arboard::ImageData { width: w, height: h, bytes: rgba.into_raw().into() }).is_ok()
    } else {
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        !entries.is_empty() && c.set().file_list(&entries).is_ok()
    };
    if set {
        *ours = Ours { text: None, seq: change_count() };
        if ours.seq.is_none() {
            quiet(2500);
        }
    }
    set
}
