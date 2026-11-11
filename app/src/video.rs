// Viewer side: a decoder thread per session turns H.264 access units into the latest
// picture, which the UI draws. Pictures stay in the decoder's own format (NV12 or I420
// YCbCr, or RGBA): the GPU converts them while drawing (video_gl.rs), so no frame is
// converted or copied on the CPU on its way to the screen. Behind on decoding, it skips
// delta frames until the next keyframe instead of building up latency.
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::media::decode::Decoder;

/// How YCbCr maps to RGB: the matrix (BT.601 or BT.709) and whether luma spans 16–235
/// (limited, what encoders write by default) or 0–255 (full).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub bt709: bool,
    pub full: bool,
}

/// Where the planes sit in `Picture::data`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Planes {
    /// Y rows, then interleaved Cb/Cr rows at half width and height
    Nv12 { y_stride: usize, uv_offset: usize, uv_stride: usize },
    /// Y, Cb and Cr planes, the chroma ones at half width and height
    I420 { y_stride: usize, u_offset: usize, v_offset: usize, c_stride: usize },
    /// 4 bytes a pixel, rows of exactly `width * 4`
    Rgba,
}

pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub planes: Planes,
    pub data: Vec<u8>,
    pub color: Color,
}

/// Integer YCbCr → RGB coefficients (×256): luma scale, Cr→R, Cb→G, Cr→G, Cb→B.
fn coefficients(c: Color) -> (i32, i32, i32, i32, i32) {
    match (c.bt709, c.full) {
        (false, false) => (298, 409, 100, 208, 516),
        (true, false) => (298, 459, 55, 136, 541),
        (false, true) => (256, 359, 88, 183, 454),
        (true, true) => (256, 403, 48, 120, 475),
    }
}

impl Picture {
    /// The picture as RGBA, converted on the CPU when it is YCbCr (screenshots, thumbnails,
    /// the camera window and renderers without the YCbCr shader).
    pub fn rgba(&self) -> Cow<'_, [u8]> {
        if self.planes == Planes::Rgba {
            return Cow::Borrowed(&self.data[..self.width * self.height * 4]);
        }
        let mut out = Vec::new();
        self.yuv_to_rgba(&mut out);
        Cow::Owned(out)
    }

    fn yuv_to_rgba(&self, out: &mut Vec<u8>) {
        let (w, h) = (self.width, self.height);
        out.resize(w * h * 4, 0);
        let (ky, rv, gu, gv, bu) = coefficients(self.color);
        let y0 = if self.color.full { 0 } else { 16 };
        let d = &self.data;
        // (luma row, chroma row → (Cb at x, Cr at x))
        let row = |y: usize, orow: &mut [u8]| {
            let (yr, cb, cr, step): (&[u8], &[u8], &[u8], usize) = match self.planes {
                Planes::Nv12 { y_stride, uv_offset, uv_stride } => {
                    let uv = &d[uv_offset + (y / 2) * uv_stride..];
                    (&d[y * y_stride..], uv, &uv[1..], 2)
                }
                Planes::I420 { y_stride, u_offset, v_offset, c_stride } => (&d[y * y_stride..], &d[u_offset + (y / 2) * c_stride..], &d[v_offset + (y / 2) * c_stride..], 1),
                Planes::Rgba => unreachable!(),
            };
            for x in 0..w {
                let c = (yr[x] as i32 - y0) * ky;
                let u = cb[(x / 2) * step] as i32 - 128;
                let v = cr[(x / 2) * step] as i32 - 128;
                let px = &mut orow[x * 4..x * 4 + 4];
                px[0] = ((c + rv * v + 128) >> 8).clamp(0, 255) as u8;
                px[1] = ((c - gu * u - gv * v + 128) >> 8).clamp(0, 255) as u8;
                px[2] = ((c + bu * u + 128) >> 8).clamp(0, 255) as u8;
                px[3] = 255;
            }
        };
        // bands of whole row pairs on several threads (a 4K picture is 33 MB of RGBA)
        let threads = if w * h < 640 * 480 { 1 } else { std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4) };
        let band = (h.div_ceil(threads) + 1) & !1;
        std::thread::scope(|sc| {
            for (i, chunk) in out.chunks_mut(band * w * 4).enumerate() {
                let row = &row;
                sc.spawn(move || {
                    for (r, orow) in chunk.chunks_exact_mut(w * 4).enumerate() {
                        row(i * band + r, orow);
                    }
                });
            }
        });
    }
}

/// The remote screen as the UI draws it: the YCbCr shader (video_gl.rs) where OpenGL 3
/// is there, otherwise an egui texture of the picture converted to RGBA.
pub struct View {
    /// the picture's size in pixels
    pub size: [usize; 2],
    pic: Option<Arc<Picture>>,
    #[cfg(not(target_os = "ios"))]
    gl: Option<Arc<crate::video_gl::GlVideo>>,
    tex: Option<egui::TextureHandle>,
    name: String,
}

impl View {
    pub fn new(name: String) -> Self {
        View {
            size: [0, 0],
            pic: None,
            #[cfg(not(target_os = "ios"))]
            gl: Some(crate::video_gl::GlVideo::new()),
            tex: None,
            name,
        }
    }

    /// The newest picture (kept for screenshots and thumbnails).
    pub fn picture(&self) -> Option<&Arc<Picture>> {
        self.pic.as_ref()
    }

    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::Vec2::new(self.size[0] as f32, self.size[1] as f32)
    }

    fn gpu(&self) -> bool {
        #[cfg(not(target_os = "ios"))]
        if let Some(gl) = &self.gl {
            return !gl.failed();
        }
        false
    }

    /// Takes a new picture. Returns the buffer of the one it replaced, for the decoder to
    /// fill again.
    pub fn set(&mut self, ctx: &egui::Context, pic: Picture) -> Option<Vec<u8>> {
        self.size = [pic.width, pic.height];
        let pic = Arc::new(pic);
        if self.gpu() {
            #[cfg(not(target_os = "ios"))]
            if let Some(gl) = &self.gl {
                gl.set(pic.clone());
            }
        } else {
            self.texture(ctx, &pic);
        }
        self.pic.replace(pic).and_then(|old| Arc::try_unwrap(old).ok()).map(|p| p.data)
    }

    fn texture(&mut self, ctx: &egui::Context, pic: &Picture) {
        let t0 = Instant::now();
        // opaque pixels: premultiplied and unmultiplied are the same, minus the per-pixel math
        let img = egui::ColorImage::from_rgba_premultiplied([pic.width, pic.height], &pic.rgba());
        crate::perf::add("colorimage", t0.elapsed());
        match &mut self.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture(&self.name, img, egui::TextureOptions::LINEAR)),
        }
    }

    /// Draws the picture into `rect` (clipped by the painter).
    pub fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        if self.gpu() {
            #[cfg(not(target_os = "ios"))]
            if let Some(gl) = &self.gl {
                painter.add(gl.callback(rect));
            }
            return;
        }
        // the GPU path gave up (an old GL): draw the newest picture as a texture from now on
        if self.tex.is_none() {
            if let Some(pic) = self.pic.clone() {
                self.texture(painter.ctx(), &pic);
            }
        }
        if let Some(t) = &self.tex {
            painter.image(t.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
        }
    }
}

#[derive(Default)]
pub struct Shared {
    /// newest decoded picture, taken by the UI
    pub latest: Mutex<Option<Picture>>,
    /// buffers of pictures the UI is done with, for the decoder to fill again (a 4K frame
    /// is 12 MB: allocating one per frame costs page faults)
    pub spare: Mutex<Vec<Vec<u8>>>,
    /// size of the last picture (stays after `latest` is taken)
    pub size: Mutex<(usize, usize)>,
    /// what decodes the video ("VideoToolbox", "OpenH264", …)
    pub decoder: Mutex<&'static str>,
    times: Mutex<Vec<Instant>>,
    pub bytes: std::sync::atomic::AtomicU64,
    queued: std::sync::atomic::AtomicUsize,
    /// a frame was dropped on a full queue: the next ones refer to it, so the decoder waits
    /// for a keyframe (decoding on would paint the changes over a broken picture)
    lost: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    /// A picture's buffer the UI no longer needs, for the decoder to fill again.
    pub fn give_back(&self, buf: Vec<u8>) {
        let mut spare = self.spare.lock().unwrap();
        if spare.len() < 3 {
            spare.push(buf);
        }
    }

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
        Self::spawn(ask_key, false)
    }

    /// The other side's camera picture (the test hooks below stay with the screen).
    pub fn start_camera(ask_key: impl Fn() + Send + 'static) -> Feed {
        Self::spawn(ask_key, true)
    }

    fn spawn(ask_key: impl Fn() + Send + 'static, camera: bool) -> Feed {
        let (tx, rx) = sync_channel::<(bool, Vec<u8>)>(16);
        let shared = Arc::new(Shared::default());
        let sh = shared.clone();
        std::thread::Builder::new().name(if camera { "decode-camera" } else { "decode" }.into()).spawn(move || run(rx, sh, ask_key, camera)).ok();
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
                self.shared.lost.store(true, Ordering::Relaxed);
                crate::perf::tick("view-queuedrop");
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
fn snap_hook(pic: &Picture) {
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
        *l.lock().unwrap() = image::RgbaImage::from_raw(pic.width as u32, pic.height as u32, pic.rgba().into_owned());
    }
}

fn run(rx: Receiver<(bool, Vec<u8>)>, sh: Arc<Shared>, ask_key: impl Fn(), camera: bool) {
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
        if sh.lost.swap(false, Ordering::Relaxed) {
            need_key = true;
        }
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
        let buf = sh.spare.lock().unwrap().pop().unwrap_or_default();
        match dec.decode(&data, buf) {
            Ok(Some(pic)) => {
                need_key = false;
                let t1 = Instant::now();
                *sh.size.lock().unwrap() = (pic.width, pic.height);
                if camera {
                    camera_dump(&pic);
                } else {
                    snap_hook(&pic);
                    dump_frame(&pic);
                }
                // an older picture the UI never took goes back to the pool
                if let Some(old) = sh.latest.lock().unwrap().replace(pic) {
                    sh.give_back(old.data);
                }
                sh.times.lock().unwrap().push(Instant::now());
                crate::perf::add("decode", t1 - t0);
                crate::perf::add("handoff", t1.elapsed());
                crate::bus::wake();
            }
            Ok(None) => {}
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
fn dump_frame(pic: &Picture) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let Some(path) = PATH.get_or_init(|| std::env::var("MANYDESK_DUMPFRAME").ok()) else { return };
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    if let Some(img) = image::RgbaImage::from_raw(pic.width as u32, pic.height as u32, pic.rgba().into_owned()) {
        let tmp = format!("{path}.tmp.png");
        if img.save(&tmp).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Test hook: MANYDESK_CAMDUMP=<file.png> writes the other side's camera picture there
/// about once a second (and MANYDESK_DEBUG logs that pictures arrive).
fn camera_dump(pic: &Picture) {
    let (w, h) = (pic.width, pic.height);
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n % 30 == 0 && std::env::var_os("MANYDESK_DEBUG").is_some() {
        eprintln!("[camera] decoded {} pictures from the other side ({w}x{h})", n + 1);
    }
    let Some(path) = PATH.get_or_init(|| std::env::var("MANYDESK_CAMDUMP").ok()) else { return };
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, pic.rgba().into_owned()) {
        let tmp = format!("{path}.tmp.png");
        if img.save(&tmp).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}
