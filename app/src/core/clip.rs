// Clipboard beyond text: pictures and copied files go to the other side's clipboard too.
// They travel over the file stream as a small batch whose id starts with "clip-img-" or
// "clip-files-"; the receiver keeps them in a temp folder and puts that on its clipboard
// (Ctrl+V / Cmd+V then pastes them there). Text stays on the control stream ("clip").
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;

/// Files on the clipboard are sent along only up to this much.
const MAX_FILES_BYTES: u64 = 256 << 20;
const MAX_FILES: usize = 2000;
/// A picture bigger than this (RGBA) is not sent.
const MAX_IMAGE_BYTES: usize = 64 << 20;

/// Set by the core: some live session shares the clipboard (pictures and files are only
/// looked at then — reading a big picture every poll is not free).
pub static CLIP_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Until this time (ms since 1970) clipboard changes are ours (set from the other side)
/// and are not sent back.
static QUIET_UNTIL: AtomicU64 = AtomicU64::new(0);

fn quiet(ms: u64) {
    QUIET_UNTIL.store(rv::now_ms() as u64 + ms, Ordering::SeqCst);
}

fn is_quiet() -> bool {
    (rv::now_ms() as u64) < QUIET_UNTIL.load(Ordering::SeqCst)
}

pub fn set_text(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        quiet(1500);
        let _ = c.set_text(text.to_string());
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
fn prune(keep: &Path) {
    let Ok(rd) = std::fs::read_dir(clip_root()) else { return };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = rd.filter_map(|e| e.ok()).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).filter(|(_, p)| p != keep).collect();
    dirs.sort();
    let n = dirs.len().saturating_sub(2);
    for (_, p) in dirs.into_iter().take(n) {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// Polls the clipboard (no portable change notification) and reports changes.
pub fn start_watch() {
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(|| {
            let Ok(mut c) = arboard::Clipboard::new() else { return };
            let mut last_text = c.get_text().unwrap_or_default();
            let mut last_files: Vec<PathBuf> = c.get().file_list().unwrap_or_default();
            let mut last_img: Option<u64> = None;
            let mut tick = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(700));
                tick += 1;
                let quiet = is_quiet();
                let active = CLIP_ACTIVE.load(Ordering::Relaxed);
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
                        if !quiet {
                            bus::emit("clip", json!({ "text": t }));
                        }
                    }
                    continue;
                }
                // a picture: looked at every ~2 s
                if !active || tick % 3 != 0 {
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
fn signature(w: usize, h: usize, px: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (w, h, px.len()).hash(&mut hs);
    let step = (px.len() / 4096).max(1);
    px.iter().step_by(step).for_each(|b| b.hash(&mut hs));
    hs.finish()
}

impl Core {
    /// Sessions the clipboard goes to: hosts that allow it, and the session in front.
    pub(super) fn clip_targets(&self, files: bool) -> Vec<Option<u32>> {
        let mut out: Vec<Option<u32>> = self.hosts.iter().filter(|h| h.state == HostState::Live && h.perms.clipboard && (!files || h.perms.files)).map(|h| h.sid).collect();
        if let Some(t) = self.active.and_then(|k| self.tabs.iter().find(|t| t.key == k)) {
            if t.state == TabState::Live && t.remote.clipboard && (!files || t.remote.files) {
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
                let (bytes, count) = tokio::task::spawn_blocking(move || fm::walk(&p).iter().fold((0u64, 0usize), |(b, n), (path, _, d)| (b + if *d { 0 } else { std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) }, n + 1))).await.unwrap_or((u64::MAX, 0));
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

    /// Whether this side takes a clipboard batch from session `sid`.
    pub(super) fn clip_allowed(&self, sid: u32, batch: &str) -> bool {
        let files = batch.starts_with("clip-files-");
        if let Some(t) = self.tabs.iter().find(|t| t.sid == Some(sid)) {
            return t.remote.clipboard && (!files || t.remote.files);
        }
        self.hosts.iter().any(|h| h.sid == Some(sid) && h.state == HostState::Live && h.perms.clipboard && (!files || h.perms.files))
    }

    /// A clipboard batch arrived complete: put it on this computer's clipboard (off the UI
    /// thread — decoding a big picture takes a while).
    pub(super) fn clip_received(&mut self, batch: &str) {
        let batch = batch.to_string();
        std::thread::spawn(move || put_received(&batch));
    }
}

fn put_received(batch: &str) {
    let Some(dir) = clip_dir(batch) else { return };
    prune(&dir);
    let Ok(mut c) = arboard::Clipboard::new() else { return };
    quiet(2500);
    if batch.starts_with("clip-img-") {
        let Some(png) = std::fs::read_dir(&dir).ok().and_then(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).next()) else { return };
        if let Ok(img) = image::open(&png) {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let _ = c.set_image(arboard::ImageData { width: w, height: h, bytes: rgba.into_raw().into() });
        }
    } else {
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        if !entries.is_empty() {
            let _ = c.set().file_list(&entries);
        }
    }
}
