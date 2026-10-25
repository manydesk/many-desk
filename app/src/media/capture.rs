// Native screen capture.
//  Linux/X11: MIT-SHM GetImage of the root window with the cursor drawn in (XFixes).
//  Linux/Wayland: the desktop portal (PipeWire) through xcap's recorder.
//  macOS / Windows: xcap's recorder (AVFoundation / DXGI) for now.
use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
#[cfg(target_os = "linux")]
use x11rb::{
    connection::Connection,
    protocol::shm::ConnectionExt as _,
    protocol::xfixes::ConnectionExt as _,
    protocol::xproto::{ConnectionExt as _, ImageFormat, Window},
    rust_connection::RustConnection,
};

/// A captured frame: BGRA (X11) or RGBA (Wayland), `stride` bytes per row.
pub struct Frame<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bgra: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Screen {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// pixels per point (2 on Retina); input on macOS is in points
    pub scale: f64,
    pub thumb: String,
}

pub fn wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false)
            || std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false))
}

/// Monitors, with a small JPEG thumbnail each when `thumbs` (none on Wayland: the portal
/// picks there). Thumbnails capture every display, which can take seconds: not on the UI thread.
pub fn screens(thumbs: bool) -> Vec<Screen> {
    #[cfg(target_os = "linux")]
    if !wayland() {
        if let Some(list) = x11_monitors(thumbs).filter(|l| !l.is_empty()) {
            return list;
        }
    }
    let Ok(monitors) = xcap::Monitor::all() else { return vec![] };
    monitors
        .iter()
        .filter_map(|m| {
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let (mut width, mut height) = (m.width().ok()?, m.height().ok()?);
            // macOS reports points; the encoder works in pixels.
            #[cfg(target_os = "macos")]
            if let Some((w, h)) = m.id().ok().and_then(super::mac::pixel_size_of) {
                (width, height) = (w as u32, h as u32);
            }
            let thumb = if wayland() || !thumbs {
                String::new()
            } else {
                // macOS: not xcap's capture_image (see mac::screenshot)
                #[cfg(target_os = "macos")]
                let img = m.id().ok().and_then(|id| super::mac::screenshot(id, 960));
                #[cfg(not(target_os = "macos"))]
                let img = m.capture_image().ok();
                img.map(thumb_url).unwrap_or_default()
            };
            Some(Screen {
                id: m.id().ok()?,
                name: m.friendly_name().or_else(|_| m.name()).unwrap_or_default(),
                x: m.x().unwrap_or(0),
                y: m.y().unwrap_or(0),
                width,
                height,
                scale: m.scale_factor().unwrap_or(1.0) as f64,
                thumb,
            })
        })
        .collect()
}

/// X11: the monitors as the desktop defines them (RandR 1.5 GetMonitors, which also
/// covers `xrandr --setmonitor` and split / tiled displays), each with a thumbnail.
#[cfg(target_os = "linux")]
fn x11_monitors(thumbs: bool) -> Option<Vec<Screen>> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let (conn, num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[num].root;
    let reply = conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
    let mut out = vec![];
    for m in reply.monitors {
        let name = conn.get_atom_name(m.name).ok().and_then(|c| c.reply().ok()).map(|r| String::from_utf8_lossy(&r.name).to_string()).unwrap_or_default();
        let mut screen = Screen { id: m.name, name, x: m.x as i32, y: m.y as i32, width: m.width as u32, height: m.height as u32, scale: 1.0, thumb: String::new() };
        if let Some(mut src) = X11Source::open(Some(&screen)).ok().filter(|_| thumbs) {
            if let Some(f) = src.grab() {
                let mut rgba = Vec::with_capacity(f.width * f.height * 4);
                for row in f.data.chunks(f.stride).take(f.height) {
                    for p in row[..f.width * 4].chunks_exact(4) {
                        rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
                    }
                }
                if let Some(img) = image::RgbaImage::from_raw(f.width as u32, f.height as u32, rgba) {
                    screen.thumb = thumb_url(img);
                }
            }
        }
        out.push(screen);
    }
    Some(out)
}

fn thumb_url(img: image::RgbaImage) -> String {
    let small = image::imageops::thumbnail(&img, 320, (320 * img.height() / img.width().max(1)).max(1));
    let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
    let mut out = Cursor::new(Vec::new());
    if rgb.write_to(&mut out, image::ImageFormat::Jpeg).is_err() {
        return String::new();
    }
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(out.into_inner()))
}

pub enum Source {
    #[cfg(target_os = "linux")]
    X11(X11Source),
    Recorder(RecorderSource),
    Synth(SynthSource),
}

impl Source {
    pub fn open(screen: Option<&Screen>) -> Result<Self, String> {
        if std::env::var_os("MANYDESK_SYNTH").is_some() {
            return Ok(Source::Synth(SynthSource::new()));
        }
        #[cfg(target_os = "linux")]
        if !wayland() {
            return X11Source::open(screen).map(Source::X11);
        }
        RecorderSource::open(screen).map(Source::Recorder)
    }

    /// The current screen contents (waits at most `wait` for a new frame on Wayland).
    pub fn grab(&mut self, wait: Duration) -> Option<Frame<'_>> {
        match self {
            #[cfg(target_os = "linux")]
            Source::X11(s) => s.grab(),
            Source::Recorder(s) => s.grab(wait),
            Source::Synth(s) => s.grab(),
        }
    }
}

/// Test hook: MANYDESK_SYNTH=1 replaces the screen with a 1920×1080 test picture — fine
/// text-like lines and edges (hard to encode, easy to see blur in) with a block moving
/// across it — so the video path can be measured with no display (locked screen, CI).
pub struct SynthSource {
    base: Vec<u8>,
    buf: Vec<u8>,
    n: usize,
    t0: Instant,
    w: usize,
    h: usize,
}

impl SynthSource {
    /// MANYDESK_SYNTH=WxH picks the size (default 1920x1080).
    fn new() -> Self {
        let size = std::env::var("MANYDESK_SYNTH").ok().and_then(|v| {
            let (a, b) = v.split_once('x')?;
            Some((a.parse::<usize>().ok()?.clamp(320, 3840) & !1, b.parse::<usize>().ok()?.clamp(240, 2160) & !1))
        });
        let (w, h) = size.unwrap_or((1920, 1080));
        let mut base = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // rows of "text": 1-px strokes in 14-px lines, varying per column block
                let line = y % 18;
                let glyph = (x / 7 + y / 18 * 13) % 11;
                let ink = line >= 3 && line <= 13 && ((x % 7 == glyph % 5) || (line == 3 + glyph % 9 && x % 7 < 5));
                let shade = if ink { 30 } else if (x / 240 + y / 270) % 2 == 0 { 250 } else { 238 };
                base[i..i + 4].copy_from_slice(&[shade, shade, shade, 255]);
            }
        }
        SynthSource { buf: base.clone(), base, n: 0, t0: Instant::now(), w, h }
    }

    fn grab(&mut self) -> Option<Frame<'_>> {
        let (w, h) = (self.w, self.h);
        // repaint where the block was, then draw it at its new place (moves 30 px/frame)
        self.buf.copy_from_slice(&self.base);
        self.n += 1;
        let bx = (self.n * 30) % (w - 200);
        let by = 400 + ((self.t0.elapsed().as_secs_f64() * 2.0).sin() * 200.0) as usize;
        for y in by..(by + 160).min(h) {
            for x in bx..bx + 200 {
                let i = (y * w + x) * 4;
                self.buf[i..i + 4].copy_from_slice(&[40, 120, 230, 255]);
            }
        }
        Some(Frame { data: &self.buf, width: w, height: h, stride: w * 4, bgra: true })
    }
}

// ---- X11 ----

#[cfg(target_os = "linux")]
pub struct X11Source {
    conn: RustConnection,
    root: Window,
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    seg: u32,
    addr: *mut u8,
    shmid: i32,
    fallback: Vec<u8>,
    use_shm: bool,
}

// The SHM mapping is owned by this struct and only touched from its capture thread.
#[cfg(target_os = "linux")]
unsafe impl Send for X11Source {}

#[cfg(target_os = "linux")]
impl X11Source {
    fn open(screen: Option<&Screen>) -> Result<Self, String> {
        let (conn, num) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let s = &conn.setup().roots[num];
        let root = s.root;
        let (x, y, w, h) = match screen {
            Some(m) => (m.x as i16, m.y as i16, m.width as u16, m.height as u16),
            None => (0, 0, s.width_in_pixels, s.height_in_pixels),
        };
        let mut src = X11Source { conn, root, x, y, w, h, seg: 0, addr: std::ptr::null_mut(), shmid: -1, fallback: vec![], use_shm: false };
        src.use_shm = src.attach_shm().is_ok();
        let _ = src.conn.xfixes_query_version(5, 0).map(|c| c.reply());
        Ok(src)
    }

    fn attach_shm(&mut self) -> Result<(), String> {
        self.conn.shm_query_version().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
        let size = self.w as usize * self.h as usize * 4;
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
            if id < 0 {
                return Err("shmget".into());
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                return Err("shmat".into());
            }
            let seg = self.conn.generate_id().map_err(|e| e.to_string())?;
            let ok = self.conn.shm_attach(seg, id as u32, false).map_err(|e| e.to_string())?.check().is_ok();
            // Marked for removal now; it lives until both sides detach.
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            if !ok {
                libc::shmdt(addr);
                return Err("shm_attach".into());
            }
            self.seg = seg;
            self.addr = addr as *mut u8;
            self.shmid = id;
        }
        Ok(())
    }

    fn grab(&mut self) -> Option<Frame<'_>> {
        let (w, h) = (self.w as usize, self.h as usize);
        if self.use_shm {
            let r = self.conn.shm_get_image(self.root, self.x, self.y, self.w, self.h, !0, ImageFormat::Z_PIXMAP.into(), self.seg, 0).ok()?;
            r.reply().ok()?;
        } else {
            let r = self.conn.get_image(ImageFormat::Z_PIXMAP, self.root, self.x, self.y, self.w, self.h, !0).ok()?.reply().ok()?;
            self.fallback = r.data;
        }
        self.draw_cursor();
        let data: &[u8] = if self.use_shm { unsafe { std::slice::from_raw_parts(self.addr, w * h * 4) } } else { &self.fallback };
        Some(Frame { data, width: w, height: h, stride: w * 4, bgra: true })
    }

    // X11 captures leave the pointer out; blend the current cursor image in.
    fn draw_cursor(&mut self) {
        let Ok(c) = self.conn.xfixes_get_cursor_image() else { return };
        let Ok(c) = c.reply() else { return };
        let (w, h) = (self.w as i32, self.h as i32);
        let buf: &mut [u8] = if self.use_shm {
            unsafe { std::slice::from_raw_parts_mut(self.addr, (w * h * 4) as usize) }
        } else {
            &mut self.fallback
        };
        let ox = c.x as i32 - c.xhot as i32 - self.x as i32;
        let oy = c.y as i32 - c.yhot as i32 - self.y as i32;
        for cy in 0..c.height as i32 {
            let py = oy + cy;
            if py < 0 || py >= h {
                continue;
            }
            for cx in 0..c.width as i32 {
                let px = ox + cx;
                if px < 0 || px >= w {
                    continue;
                }
                let argb = c.cursor_image[(cy * c.width as i32 + cx) as usize];
                let a = argb >> 24;
                if a == 0 {
                    continue;
                }
                // Premultiplied ARGB over BGRA.
                let i = ((py * w + px) * 4) as usize;
                let inv = 255 - a;
                buf[i] = ((argb & 0xff) + (buf[i] as u32 * inv) / 255).min(255) as u8;
                buf[i + 1] = (((argb >> 8) & 0xff) + (buf[i + 1] as u32 * inv) / 255).min(255) as u8;
                buf[i + 2] = (((argb >> 16) & 0xff) + (buf[i + 2] as u32 * inv) / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for X11Source {
    fn drop(&mut self) {
        if self.use_shm {
            let _ = self.conn.shm_detach(self.seg);
            let _ = self.conn.flush();
            unsafe { libc::shmdt(self.addr as *const libc::c_void) };
        }
    }
}

// ---- xcap recorder: Wayland portal, macOS, Windows (RGBA frames) ----

/// One xcap recorder per monitor, shared by every session that shows it: Windows allows a
/// single desktop duplication per output, and on Wayland each recorder is a portal prompt.
struct Hub {
    rec: xcap::VideoRecorder,
    latest: std::sync::Mutex<(u64, Option<Arc<xcap::Frame>>)>,
    cond: std::sync::Condvar,
    users: std::sync::atomic::AtomicUsize,
}

// xcap's recorder handle only signals its worker thread.
unsafe impl Send for Hub {}
unsafe impl Sync for Hub {}

static HUBS: std::sync::Mutex<Vec<(u32, Arc<Hub>)>> = std::sync::Mutex::new(Vec::new());

pub struct RecorderSource {
    hub: Arc<Hub>,
    id: u32,
    seen: u64,
    last: Option<Arc<xcap::Frame>>,
}

impl RecorderSource {
    fn open(screen: Option<&Screen>) -> Result<Self, String> {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let m = screen
            .and_then(|s| monitors.iter().find(|m| m.id().ok() == Some(s.id)))
            .or_else(|| monitors.first())
            .ok_or("no monitor")?;
        let id = m.id().map_err(|e| e.to_string())?;
        let mut hubs = HUBS.lock().unwrap();
        if let Some((_, hub)) = hubs.iter().find(|h| h.0 == id) {
            hub.users.fetch_add(1, Ordering::SeqCst);
            return Ok(RecorderSource { hub: hub.clone(), id, seen: 0, last: None });
        }
        let (rec, rx) = m.video_recorder().map_err(|e| e.to_string())?;
        rec.start().map_err(|e| e.to_string())?;
        let hub = Arc::new(Hub { rec, latest: Default::default(), cond: Default::default(), users: 1.into() });
        let h = Arc::downgrade(&hub);
        std::thread::Builder::new()
            .name("capture-hub".into())
            .spawn(move || {
                while let Ok(f) = rx.recv() {
                    let Some(h) = h.upgrade() else { break };
                    let mut l = h.latest.lock().unwrap();
                    l.0 += 1;
                    l.1 = Some(Arc::new(f));
                    h.cond.notify_all();
                }
            })
            .map_err(|e| e.to_string())?;
        hubs.push((id, hub.clone()));
        Ok(RecorderSource { hub, id, seen: 0, last: None })
    }

    fn grab(&mut self, wait: Duration) -> Option<Frame<'_>> {
        // The newest frame; the recorder delivers on change.
        let l = self.hub.latest.lock().unwrap();
        let (l, _) = self.hub.cond.wait_timeout_while(l, wait, |l| l.0 == self.seen).unwrap();
        if l.0 != self.seen {
            self.seen = l.0;
            self.last = l.1.clone();
        }
        drop(l);
        let f = self.last.as_ref()?;
        Some(Frame { data: &f.raw, width: f.width as usize, height: f.height as usize, stride: f.width as usize * 4, bgra: false })
    }
}

impl Drop for RecorderSource {
    fn drop(&mut self) {
        // Under the registry lock, so a session opening right now cannot join a stopping hub.
        let mut hubs = HUBS.lock().unwrap();
        if self.hub.users.fetch_sub(1, Ordering::SeqCst) == 1 {
            hubs.retain(|h| h.0 != self.id || !Arc::ptr_eq(&h.1, &self.hub));
            let _ = self.hub.rec.stop();
        }
    }
}

/// BGRA/RGBA → I420 at (dw, dh), nearest-neighbour scaled (BT.601 limited range).
/// BGRA/RGBA → I420 at `dw`×`dh` (BT.601, limited range). Scaling averages every source
/// pixel a destination pixel covers (an area filter) — picking one of them instead, as
/// before, dropped thin text strokes and left the rest jagged. Chroma is the 2×2 average.
/// Bands of rows run in parallel: the encoder is single-threaded, the other cores idle.
pub fn to_i420(f: &Frame<'_>, dw: usize, dh: usize, out: &mut Vec<u8>) {
    let (cw, ch) = (dw / 2, dh / 2);
    out.resize(dw * dh + 2 * cw * ch, 0);
    let (y_plane, uv) = out.split_at_mut(dw * dh);
    let (u_plane, v_plane) = uv.split_at_mut(cw * ch);
    let span = |d: usize, dn: usize, sn: usize| {
        let a = (d * sn / dn).min(sn - 1);
        let b = ((d + 1) * sn / dn).clamp(a + 1, sn);
        (a, b)
    };
    let cols: Vec<(usize, usize)> = (0..dw).map(|x| span(x, dw, f.width)).collect();
    // below 2×: every column 1–2 pixels wide. (byte offset, weight of that pixel, weight of
    // the next): a 1-px last column reads the pair ending on it, so 8 bytes stay in the row.
    let narrow = cols.iter().all(|&(a, b)| b - a <= 2) && f.width >= 2;
    let pairs: Vec<(usize, u32, u32)> = cols
        .iter()
        .map(|&(a, b)| if a + 1 >= f.width { ((f.width - 2) * 4, 0, 1) } else { (a * 4, 1, (b - a == 2) as u32) })
        .collect();
    let rows: Vec<(usize, usize)> = (0..dh).map(|y| span(y, dh, f.height)).collect();
    let job = Job { f, dw, cw, cols: &cols, pairs: &pairs, rows: &rows, narrow, one_to_one: dw == f.width && dh == f.height };
    // bands of whole chroma rows (pairs of luma rows)
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8).min(ch.max(1));
    let band = ch.div_ceil(threads).max(1); // chroma rows per band
    if threads == 1 {
        job.run(0, dh, y_plane, u_plane, v_plane);
        return;
    }
    std::thread::scope(|sc| {
        let ys = y_plane.chunks_mut(band * 2 * dw);
        let us = u_plane.chunks_mut(band * cw);
        let vs = v_plane.chunks_mut(band * cw);
        for (k, ((yb, ub), vb)) in ys.zip(us).zip(vs).enumerate() {
            let (y0, y1) = (k * band * 2, ((k + 1) * band * 2).min(dh));
            let job = &job;
            sc.spawn(move || job.run(y0, y1, yb, ub, vb));
        }
    });
}

struct Job<'a, 'b> {
    f: &'a Frame<'b>,
    dw: usize,
    cw: usize,
    cols: &'a [(usize, usize)],
    pairs: &'a [(usize, u32, u32)],
    rows: &'a [(usize, usize)],
    narrow: bool,
    one_to_one: bool,
}

impl Job<'_, '_> {
    /// Destination rows `y0..y1` (y0 even) into their slices of the planes.
    fn run(&self, y0: usize, y1: usize, y_plane: &mut [u8], u_plane: &mut [u8], v_plane: &mut [u8]) {
        let (f, dw, cw) = (self.f, self.dw, self.cw);
        let (ri, bi) = if f.bgra { (2, 0) } else { (0, 2) };
        let luma = |r: u32, g: u32, b: u32| (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        let chroma = |r: i32, g: i32, b: i32| ((((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8, (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8);
        // two destination rows of RGB (averaged), for the 2×2 chroma
        let mut sums = vec![[0u32; 3]; dw * 2];
        for y in y0..y1 {
            let (s0, s1) = self.rows[y];
            let acc = &mut sums[(y % 2) * dw..(y % 2 + 1) * dw];
            let yrow = &mut y_plane[(y - y0) * dw..(y - y0 + 1) * dw];
            if self.one_to_one {
                let row = &f.data[y * f.stride..y * f.stride + f.width * 4];
                for ((a, yv), p) in acc.iter_mut().zip(yrow.iter_mut()).zip(row.chunks_exact(4)) {
                    let (r, g, b) = (p[ri] as u32, p[1] as u32, p[bi] as u32);
                    *a = [r, g, b];
                    *yv = luma(r, g, b);
                }
            } else {
                acc.fill([0; 3]);
                // source rows in order (cache friendly), each added into the columns it feeds
                for sy in s0..s1 {
                    let row = &f.data[sy * f.stride..sy * f.stride + f.width * 4];
                    if self.narrow {
                        // one or two source pixels per column, summed without a branch
                        for (a, &(o, w0, w1)) in acc.iter_mut().zip(self.pairs.iter()) {
                            let p = &row[o..o + 8];
                            a[0] += w0 * p[ri] as u32 + w1 * p[4 + ri] as u32;
                            a[1] += w0 * p[1] as u32 + w1 * p[5] as u32;
                            a[2] += w0 * p[bi] as u32 + w1 * p[4 + bi] as u32;
                        }
                    } else {
                        for (a, &(x0, x1)) in acc.iter_mut().zip(self.cols.iter()) {
                            for p in row[x0 * 4..x1 * 4].chunks_exact(4) {
                                a[0] += p[ri] as u32;
                                a[1] += p[1] as u32;
                                a[2] += p[bi] as u32;
                            }
                        }
                    }
                }
                let n_rows = (s1 - s0) as u32;
                for ((a, yv), &(x0, x1)) in acc.iter_mut().zip(yrow.iter_mut()).zip(self.cols.iter()) {
                    let n = n_rows * (x1 - x0) as u32;
                    if n > 1 {
                        // ×(65536/n) and a shift instead of three divisions
                        let k = (65536 + n / 2) / n;
                        *a = [(a[0] * k + 32768) >> 16, (a[1] * k + 32768) >> 16, (a[2] * k + 32768) >> 16];
                    }
                    *yv = luma(a[0], a[1], a[2]);
                }
            }
            if y % 2 == 1 {
                let cy = (y - y0) / 2;
                for cx in 0..cw {
                    let mut t = [0u32; 3];
                    for (row, col) in [(0, 2 * cx), (0, 2 * cx + 1), (1, 2 * cx), (1, 2 * cx + 1)] {
                        let p = sums[row * dw + col];
                        t[0] += p[0];
                        t[1] += p[1];
                        t[2] += p[2];
                    }
                    let (u, v) = chroma(((t[0] + 2) / 4) as i32, ((t[1] + 2) / 4) as i32, ((t[2] + 2) / 4) as i32);
                    u_plane[cy * cw + cx] = u;
                    v_plane[cy * cw + cx] = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    fn frame(w: usize, h: usize, px: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                v[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&px(x, y));
            }
        }
        v
    }

    #[test]
    fn flat_colour_is_exact_at_any_scale() {
        let data = frame(64, 48, |_, _| [200, 100, 50, 255]); // BGRA
        let f = Frame { data: &data, width: 64, height: 48, stride: 256, bgra: true };
        for (dw, dh) in [(64, 48), (32, 24), (40, 30)] {
            let mut out = Vec::new();
            to_i420(&f, dw, dh, &mut out);
            let y = out[0];
            assert!(out[..dw * dh].iter().all(|&v| v == y), "{dw}x{dh}");
        }
    }

    #[test]
    fn the_last_column_is_its_own_pixel() {
        // a red right edge, 15 px wide → 10 (spans of 1 and 2): the last column stays red
        let data = frame(15, 4, |x, _| if x == 14 { [0, 0, 255, 255] } else { [255, 255, 255, 255] });
        let f = Frame { data: &data, width: 15, height: 4, stride: 60, bgra: true };
        let mut out = Vec::new();
        to_i420(&f, 10, 4, &mut out);
        // it covers the last white pixel and the red one: their average
        assert!((140..180).contains(&out[9]), "last column: average of white and red, got {}", out[9]);
        assert!(out[0] > 230);
    }

    #[test]
    #[ignore]
    fn bench_to_i420() {
        let (w, h) = (3024, 1964);
        let data = frame(w, h, |x, y| [(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255]);
        let f = Frame { data: &data, width: w, height: h, stride: w * 4, bgra: true };
        let mut out = Vec::new();
        for (dw, dh) in [(3024, 1964), (2598, 1688), (2078, 1350), (1512, 982)] {
            let t = std::time::Instant::now();
            for _ in 0..10 {
                to_i420(&f, dw, dh, &mut out);
            }
            println!("{dw}x{dh}: {:.1} ms", t.elapsed().as_secs_f64() * 100.0);
        }
    }

    #[test]
    fn a_thin_line_survives_downscaling() {
        // white with a 1-px black vertical line every 4th column, halved: nearest-neighbour
        // either lost the line entirely or kept it full black depending on the phase;
        // the area filter keeps it as a half-grey line everywhere.
        let data = frame(64, 16, |x, _| if x % 4 == 1 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let f = Frame { data: &data, width: 64, height: 16, stride: 256, bgra: true };
        let mut out = Vec::new();
        to_i420(&f, 32, 8, &mut out);
        let row = &out[..32];
        let (white, grey) = (row[1], row[0]);
        assert!(white > 230, "white {white}");
        assert!((100..160).contains(&grey), "line kept as grey: {grey}");
    }
}
