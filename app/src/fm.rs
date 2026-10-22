// File manager (AnyDesk's file transfer view): the viewer browses its own disk and the
// host's side by side and copies either way. Over the session's file stream:
//
//   viewer → host  {"t":"fls","rid","path"}            list a folder ("" = home)
//                  {"t":"fget","rid","paths":[..]}      send me these (folders recursively)
//                  {"t":"fcancel","rid"}                stop that download
//                  {"t":"fmk","rid","path"} / {"t":"frm","rid","paths"} / {"t":"fmv","rid","from","to"}
//   host → viewer  {"t":"fls","rid","path","parent","sep","entries","roots"} / {"t":"fok","rid"} /
//                  {"t":"ferr","rid","e"}
//
// Requests and answers go over the control stream; the copies themselves over the file
// stream, either way, one file at a time (Session::file_turn):
//   "fstart" {fm: batch, dir, rel, size, total, d} + chunks + "fend" {fm, x: cancelled}, then
//   "fdone" {fm, x, e} when the batch is over. `dir` is the target folder for uploads
//   (downloads land where the viewer asked); `d` marks an empty folder (no chunks).
//
// Every path the other side names for writing is checked to stay inside its folder.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::mdp::Session;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    /// name
    pub n: String,
    /// is a folder
    pub d: bool,
    /// size in bytes (files)
    pub s: u64,
    /// modified, ms since 1970
    pub m: i64,
}

/// One side of the view.
#[derive(Default)]
pub struct Pane {
    pub path: String,
    pub parent: Option<String>,
    pub sep: String,
    pub entries: Vec<Entry>,
    pub roots: Vec<String>,
    pub selected: Vec<String>,
    pub error: String,
    pub loading: bool,
}

impl Pane {
    /// From a host's "fls" answer.
    pub fn from_reply(m: &Value) -> Pane {
        let strs = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        Pane {
            path: m["path"].as_str().unwrap_or("").into(),
            parent: m["parent"].as_str().map(String::from),
            sep: m["sep"].as_str().unwrap_or("/").into(),
            entries: serde_json::from_value(m["entries"].clone()).unwrap_or_default(),
            roots: strs(&m["roots"]),
            ..Default::default()
        }
    }

    /// The selected entries' full paths.
    pub fn selected_paths(&self) -> Vec<String> {
        self.entries.iter().filter(|e| self.selected.contains(&e.n)).map(|e| self.join(&e.n)).collect()
    }

    /// A click on row `n`: alone, Ctrl/Cmd toggles, Shift extends from the last clicked row.
    pub fn click(&mut self, n: usize, toggle: bool, range: bool) {
        let Some(name) = self.entries.get(n).map(|e| e.n.clone()) else { return };
        let anchor = self.selected.last().and_then(|a| self.entries.iter().position(|e| &e.n == a));
        match (range, anchor) {
            (true, Some(a)) => {
                let (lo, hi) = (a.min(n), a.max(n));
                let mut sel: Vec<String> = self.entries[lo..=hi].iter().map(|e| e.n.clone()).collect();
                // keep the anchor last, so the next Shift-click extends from it
                sel.retain(|x| self.selected.last() != Some(x));
                sel.push(self.selected.last().unwrap().clone());
                self.selected = sel;
            }
            _ if toggle => {
                if let Some(p) = self.selected.iter().position(|x| *x == name) {
                    self.selected.remove(p);
                } else {
                    self.selected.push(name);
                }
            }
            _ => self.selected = vec![name],
        }
    }

    pub fn join(&self, name: &str) -> String {
        if self.path.ends_with(&self.sep) {
            format!("{}{name}", self.path)
        } else {
            format!("{}{}{name}", self.path, self.sep)
        }
    }
}

/// A copy in progress (or finished) shown under the panes.
#[derive(Clone, Default)]
pub struct Transfer {
    pub id: String,
    pub name: String,
    pub upload: bool,
    pub done: u64,
    pub total: u64,
    pub finished: bool,
    pub cancelled: bool,
    pub error: String,
    /// the folder it goes into (to refresh that pane when it's done)
    pub dest: String,
}

#[derive(Default)]
pub struct FileManager {
    pub open: bool,
    pub local: Pane,
    pub remote: Pane,
    pub transfers: Vec<Transfer>,
    /// per download batch: the local folder it lands in (fixed when it starts)
    pub dests: HashMap<String, PathBuf>,
    /// per upload batch: set to stop it
    pub cancels: HashMap<String, Arc<AtomicBool>>,
    next_rid: u32,
}

impl FileManager {
    pub fn rid(&mut self) -> String {
        self.next_rid += 1;
        self.next_rid.to_string()
    }

    pub fn transfer(&mut self, id: &str) -> Option<&mut Transfer> {
        self.transfers.iter_mut().find(|t| t.id == id)
    }

    pub fn busy(&self) -> bool {
        self.transfers.iter().any(|t| !t.finished)
    }

    /// Reads a local folder into the left pane.
    pub fn list_local(&mut self, path: &str) {
        let p = if path.is_empty() { home() } else { PathBuf::from(path) };
        match list(&p) {
            Ok(entries) => {
                self.local = Pane {
                    path: p.to_string_lossy().to_string(),
                    parent: p.parent().map(|x| x.to_string_lossy().to_string()),
                    sep: std::path::MAIN_SEPARATOR.to_string(),
                    entries,
                    roots: roots(),
                    ..Default::default()
                };
            }
            Err(e) => self.local.error = e,
        }
    }
}

/// "a.txt" for one selected entry, else "3 items"-style text from `many`.
pub fn summary(paths: &[String], many: impl Fn(usize) -> String) -> String {
    match paths {
        [one] => one.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(one).to_string(),
        _ => many(paths.len()),
    }
}

/// Sends `paths` (folders recursively) as batch `batch` over the file stream, into `dir`
/// on the other side (None: the other side knows where the batch goes), then "fdone".
/// `progress(done, total)` is called every ~150 ms and at the end.
pub async fn send_batch(s: Arc<Session>, paths: Vec<PathBuf>, batch: String, dir: Option<String>, cancel: Arc<AtomicBool>, progress: impl Fn(u64, u64)) -> Result<bool, String> {
    let items = tokio::task::spawn_blocking(move || walk(&paths)).await.map_err(|e| e.to_string())?;
    let total: u64 = items.iter().filter(|i| !i.2).filter_map(|i| std::fs::metadata(&i.0).ok()).map(|m| m.len()).sum();
    let r = send_items(&s, items, &batch, dir.as_deref(), total, &cancel, &progress).await;
    let cancelled = matches!(r, Ok(false));
    let e = r.as_ref().err().cloned().unwrap_or_default();
    let _ = s.send_text(true, &json!({ "t": "fdone", "fm": batch, "dir": dir, "x": cancelled, "e": e }).to_string()).await;
    r
}

async fn send_items(s: &Session, items: Vec<(PathBuf, String, bool)>, batch: &str, dir: Option<&str>, total: u64, cancel: &AtomicBool, progress: &impl Fn(u64, u64)) -> Result<bool, String> {
    let mut done = 0u64;
    let mut last = Instant::now();
    let mut buf = vec![0u8; 256 * 1024];
    for (path, rel, empty_dir) in items {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let _turn = s.file_turn().await;
        if empty_dir {
            s.send_text(true, &json!({ "t": "fstart", "fm": batch, "id": batch, "dir": dir, "rel": rel, "d": true, "total": total }).to_string()).await?;
            continue;
        }
        // unreadable files are skipped
        let Ok(mut f) = tokio::fs::File::open(&path).await else { continue };
        let size = f.metadata().await.map(|m| m.len()).unwrap_or(0);
        s.send_text(true, &json!({ "t": "fstart", "fm": batch, "id": batch, "dir": dir, "rel": rel, "size": size, "total": total }).to_string()).await?;
        let mut failed = None;
        let mut stopped = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                stopped = true;
                break;
            }
            let n = match f.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    failed = Some(e.to_string());
                    break;
                }
            };
            s.send_file_bytes(&buf[..n]).await?;
            done += n as u64;
            if last.elapsed() > Duration::from_millis(150) {
                last = Instant::now();
                progress(done, total);
            }
        }
        // "x": the receiver drops the partial file
        s.send_text(true, &json!({ "t": "fend", "fm": batch, "id": batch, "x": stopped || failed.is_some() }).to_string()).await?;
        if let Some(e) = failed {
            return Err(e);
        }
        if stopped {
            return Ok(false);
        }
    }
    progress(done, total);
    Ok(true)
}

/// Where a received entry goes: `rel` under `base` (None if it tries to leave it); files
/// get a free name when one is taken, folders are merged.
pub fn target(base: &Path, rel: &str, dir: bool) -> Option<PathBuf> {
    let p = safe_join(base, rel)?;
    if dir {
        return Some(p);
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    Some(unique(p))
}

/// Delete (folders with their contents).
pub fn remove(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) => Err(e),
    }
}

/// A plain name for a new or renamed entry: no separators, not "." or "..".
pub fn valid_name(n: &str) -> bool {
    let n = n.trim();
    !n.is_empty() && n != "." && n != ".." && !n.contains(['/', '\\', ':', '\0']) && n.len() <= 255
}

/// The host's answer to "fls".
pub fn list_reply(rid: &str, path: &str) -> Value {
    let p = if path.is_empty() { home() } else { PathBuf::from(path) };
    match list(&p) {
        Ok(entries) => json!({
            "t": "fls", "rid": rid,
            "path": p.to_string_lossy(),
            "parent": p.parent().map(|x| x.to_string_lossy().to_string()),
            "sep": std::path::MAIN_SEPARATOR.to_string(),
            "entries": entries,
            "roots": roots(),
        }),
        Err(e) => json!({ "t": "ferr", "rid": rid, "e": e }),
    }
}

pub fn home() -> PathBuf {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("/"))
}

/// Places to start from: home, and the drives (Windows) or "/".
pub fn roots() -> Vec<String> {
    let mut out = vec![home().to_string_lossy().to_string()];
    #[cfg(windows)]
    for c in b'A'..=b'Z' {
        let d = format!("{}:\\", c as char);
        if Path::new(&d).exists() {
            out.push(d);
        }
    }
    #[cfg(not(windows))]
    out.push("/".into());
    out
}

/// A folder's entries: folders first, then by name (case-insensitive).
pub fn list(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut out: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            let m = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
            Some(Entry { n: e.file_name().to_string_lossy().to_string(), d: md.is_dir(), s: if md.is_dir() { 0 } else { md.len() }, m })
        })
        .collect();
    out.sort_by(|a, b| b.d.cmp(&a.d).then_with(|| a.n.to_lowercase().cmp(&b.n.to_lowercase())));
    Ok(out)
}

/// `rel` (from the other side) under `dir`, or None if it tries to leave it.
pub fn safe_join(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.replace('\\', "/");
    let mut out = dir.to_path_buf();
    let mut any = false;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let c = Path::new(part).components().next()?;
        match c {
            Component::Normal(n) if Path::new(n).components().count() == 1 && !part.contains(':') => {
                out.push(n);
                any = true;
            }
            _ => return None,
        }
    }
    any.then_some(out)
}

/// Every file under the selected `paths` with its path relative to the selection's
/// folder (folders recursively; empty folders come back as (path, rel, true)).
pub fn walk(paths: &[PathBuf]) -> Vec<(PathBuf, String, bool)> {
    fn go(p: &Path, rel: String, out: &mut Vec<(PathBuf, String, bool)>, depth: u32) {
        if depth > 64 {
            return;
        }
        match std::fs::symlink_metadata(p) {
            Ok(md) if md.is_dir() => {
                let mut kids: Vec<_> = std::fs::read_dir(p).map(|r| r.filter_map(|e| e.ok()).collect()).unwrap_or_default();
                if kids.is_empty() {
                    out.push((p.to_path_buf(), rel, true));
                    return;
                }
                kids.sort_by_key(|e| e.file_name());
                for k in kids {
                    let name = k.file_name().to_string_lossy().to_string();
                    go(&k.path(), format!("{rel}/{name}"), out, depth + 1);
                }
            }
            Ok(md) if md.is_file() => out.push((p.to_path_buf(), rel, false)),
            _ => {} // symlinks and specials are skipped
        }
    }
    let mut out = vec![];
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !name.is_empty() {
            go(p, name, &mut out, 0);
        }
    }
    out
}

/// A free name: "a.txt" → "a (2).txt" when taken.
pub fn unique(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for i in 2..10_000 {
        let c = p.with_file_name(format!("{stem} ({i}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    p
}

pub fn human_size(n: u64) -> String {
    let f = n as f64;
    if f < 1024.0 {
        format!("{n} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / 1048576.0)
    } else {
        format!("{:.2} GB", f / 1073741824.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_stays_inside() {
        let d = Path::new("/tmp/x");
        assert_eq!(safe_join(d, "a/b.txt"), Some(PathBuf::from("/tmp/x/a/b.txt")));
        assert_eq!(safe_join(d, "a\\b.txt"), Some(PathBuf::from("/tmp/x/a/b.txt")));
        assert_eq!(safe_join(d, "../etc/passwd"), None);
        assert_eq!(safe_join(d, "a/../../b"), None);
        assert_eq!(safe_join(d, "/etc/passwd").map(|p| p.starts_with(d)), Some(true));
        assert_eq!(safe_join(d, "C:/Windows"), None);
        assert_eq!(safe_join(d, ""), None);
    }

    #[test]
    fn names_and_selection() {
        assert!(valid_name("a b.txt"));
        assert!(!valid_name("../x") && !valid_name("..") && !valid_name("") && !valid_name("a/b") && !valid_name("c:x"));
        let mut p = Pane { sep: "/".into(), path: "/d".into(), ..Default::default() };
        p.entries = ["a", "b", "c", "d"].iter().map(|n| Entry { n: n.to_string(), d: false, s: 0, m: 0 }).collect();
        p.click(1, false, false);
        p.click(3, false, true);
        let mut s = p.selected.clone();
        s.sort();
        assert_eq!(s, vec!["b", "c", "d"]);
        p.click(2, true, false);
        assert!(!p.selected.contains(&"c".to_string()));
        assert_eq!(p.selected_paths().len(), 2);
        assert_eq!(summary(&["/x/y/f.txt".into()], |n| n.to_string()), "f.txt");
        assert_eq!(summary(&["a".into(), "b".into()], |n| format!("{n} items")), "2 items");
    }

    #[test]
    fn walk_finds_nested_files() {
        let root = std::env::temp_dir().join(format!("md-walk-{}", rand::random::<u32>()));
        std::fs::create_dir_all(root.join("sel/sub/empty")).unwrap();
        std::fs::write(root.join("sel/a.txt"), b"a").unwrap();
        std::fs::write(root.join("sel/sub/b.txt"), b"bb").unwrap();
        let w = walk(&[root.join("sel")]);
        let rels: Vec<(String, bool)> = w.iter().map(|x| (x.1.clone(), x.2)).collect();
        assert_eq!(rels, vec![("sel/a.txt".into(), false), ("sel/sub/b.txt".into(), false), ("sel/sub/empty".into(), true)]);
        let _ = std::fs::remove_dir_all(root);
    }
}
