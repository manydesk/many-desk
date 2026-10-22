// Viewer side: a decoder thread per session turns H.264 access units into the latest
// RGBA picture, which the UI uploads as a texture. Behind on decoding, it skips delta
// frames until the next keyframe instead of building up latency.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::media::decode::Decoder;

pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
pub struct Shared {
    /// newest decoded picture, taken by the UI
    pub latest: Mutex<Option<Picture>>,
    /// size of the last picture (stays after `latest` is taken)
    pub size: Mutex<(usize, usize)>,
    /// what decodes the video ("VideoToolbox", "OpenH264", …)
    pub decoder: Mutex<&'static str>,
    times: Mutex<Vec<Instant>>,
    pub bytes: std::sync::atomic::AtomicU64,
    queued: std::sync::atomic::AtomicUsize,
    stop: AtomicBool,
}

impl Shared {
    pub fn fps(&self) -> f64 {
        let mut t = self.times.lock().unwrap();
        let now = Instant::now();
        t.retain(|x| now.duration_since(*x) < Duration::from_secs(2));
        t.len() as f64 / 2.0
    }
}

pub struct Feed {
    tx: SyncSender<(bool, Vec<u8>)>,
    pub shared: Arc<Shared>,
}

impl Feed {
    /// `ask_key`: request a keyframe from the sender.
    pub fn start(ask_key: impl Fn() + Send + 'static) -> Feed {
        let (tx, rx) = sync_channel::<(bool, Vec<u8>)>(16);
        let shared = Arc::new(Shared::default());
        let sh = shared.clone();
        std::thread::Builder::new().name("decode".into()).spawn(move || run(rx, sh, ask_key)).ok();
        Feed { tx, shared }
    }

    /// Called from the network thread for every access unit.
    pub fn push(&self, key: bool, data: Vec<u8>) -> bool {
        self.shared.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        // Counted before sending so the decoder never sees it negative.
        self.shared.queued.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send((key, data)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.shared.queued.fetch_sub(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Test hook: MANYDESK_SNAP=<file.png> — when `<file.png>.req` appears, the newest decoded
/// picture is written there (full size, works with the window hidden) and the .req removed.
fn snap_hook(w: usize, h: usize, rgba: &[u8]) {
    static LAST: std::sync::OnceLock<Option<Arc<Mutex<Option<image::RgbaImage>>>>> = std::sync::OnceLock::new();
    let last = LAST.get_or_init(|| {
        let path = std::env::var_os("MANYDESK_SNAP")?;
        let last: Arc<Mutex<Option<image::RgbaImage>>> = Default::default();
        let l = last.clone();
        std::thread::spawn(move || {
            let req = std::path::PathBuf::from(format!("{}.req", path.to_string_lossy()));
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if req.exists() {
                    if let Some(img) = l.lock().unwrap().clone() {
                        let _ = img.save(&path);
                    }
                    let _ = std::fs::remove_file(&req);
                }
            }
        });
        Some(last)
    });
    if let Some(l) = last {
        *l.lock().unwrap() = image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec());
    }
}

fn run(rx: Receiver<(bool, Vec<u8>)>, sh: Arc<Shared>, ask_key: impl Fn()) {
    let Ok(mut dec) = Decoder::new() else { return };
    *sh.decoder.lock().unwrap() = dec.name();
    let mut need_key = true;
    let mut last_ask = Instant::now() - Duration::from_secs(1);
    let ask = |last: &mut Instant| {
        if last.elapsed() > Duration::from_millis(300) {
            *last = Instant::now();
            ask_key();
        }
    };
    while let Ok((key, data)) = rx.recv() {
        let queued = sh.queued.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        if sh.stop.load(Ordering::Relaxed) {
            break;
        }
        // Far behind: drop deltas, resync on the next keyframe. Frames often arrive in a
        // burst after a hiccup on the network; decoding takes a few ms, so a short queue
        // drains by itself — dropping it would cost a keyframe (many times a delta's size).
        let behind = queued >= 8;
        if behind && !key {
            crate::perf::tick("view-decdrop");
            need_key = true;
            ask(&mut last_ask);
            continue;
        }
        if need_key && !key {
            ask(&mut last_ask);
            continue;
        }
        let t0 = Instant::now();
        match dec.decode(&data) {
            Ok(true) => {
                need_key = false;
                let t1 = Instant::now();
                *sh.size.lock().unwrap() = (dec.width, dec.height);
                snap_hook(dec.width, dec.height, &dec.rgba);
                *sh.latest.lock().unwrap() = Some(Picture { width: dec.width, height: dec.height, rgba: dec.rgba.clone() });
                dump_frame(&dec.rgba, dec.width, dec.height);
                sh.times.lock().unwrap().push(Instant::now());
                crate::perf::add("decode", t1 - t0);
                crate::perf::add("handoff", t1.elapsed());
                crate::bus::wake();
            }
            Ok(false) => {}
            Err(_) => {
                need_key = true;
                dec.reset();
                *sh.decoder.lock().unwrap() = dec.name();
                ask(&mut last_ask);
            }
        }
    }
}

/// Test hook: MANYDESK_DUMPFRAME=<file.png> writes the decoded picture there about once a
/// second (to compare what arrives with the host's screen).
fn dump_frame(rgba: &[u8], w: usize, h: usize) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let Some(path) = PATH.get_or_init(|| std::env::var("MANYDESK_DUMPFRAME").ok()) else { return };
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec()) {
        let tmp = format!("{path}.tmp.png");
        if img.save(&tmp).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}
