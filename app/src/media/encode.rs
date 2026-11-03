// Host video: capture → H.264 → a sink (the MDP link, or a WebRTC track on Linux).
// A still screen costs ~nothing: unchanged frames skip the encoder entirely.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;
use serde_json::{json, Value};

use super::capture::{self, Screen, Source};

enum H264 {
    Open(Encoder),
    #[cfg(windows)]
    Mf(super::win::MfEncoder),
}

pub struct EncodedFrame {
    /// Annex-B access unit
    pub data: Bytes,
    pub key: bool,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // RTP sample timing (Linux WebRTC bridge)
    pub duration: Duration,
}

pub trait Sink: Send + Sync + 'static {
    /// false: the transport is behind and dropped the frame (the next one becomes a keyframe).
    fn push(&self, frame: EncodedFrame) -> bool;
    fn closed(&self) -> bool;
    /// The path is backed up: the encoder should skip this frame instead of encoding it
    /// (a skipped frame keeps the reference chain intact; a dropped one needs a keyframe).
    fn congested(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Params {
    pub scale: f64,
    pub fps: f64,
    pub bitrate: u32,
}

#[derive(Default)]
struct Stats {
    frames: u64,
    width: usize,
    height: usize,
    window: Vec<Instant>,
    // Recent limitations (read by both the UI and the auto-quality loop, so not reset on read).
    /// a frame the transport could not take (dropped; a keyframe follows)
    last_drop: Option<Instant>,
    /// frames skipped because the path was backed up, and frames that took longer to
    /// encode than their slot — counted, so one stray event doesn't lower the quality
    skips: Vec<Instant>,
    slows: Vec<Instant>,
    /// software path: frames done well inside their slot (room to grow the picture), and
    /// how many frames were timed at all
    fast: Vec<Instant>,
    timed: Vec<Instant>,
    implementation: &'static str,
    hardware: bool,
}

pub struct Video {
    screen: Option<Screen>,
    params: Mutex<Params>,
    keyframe: AtomicBool,
    stop: AtomicBool,
    started: AtomicBool,
    /// the pointer is drawn into the picture (false: the viewer draws it from the control
    /// stream; macOS capture only)
    cursor: AtomicBool,
    stats: Mutex<Stats>,
}

impl Video {
    pub fn new(screen: Option<Screen>) -> Arc<Self> {
        Arc::new(Video {
            screen,
            params: Mutex::new(Params { scale: 1.0, fps: 30.0, bitrate: 4_000_000 }),
            keyframe: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
            cursor: AtomicBool::new(true),
            stats: Default::default(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.screen.as_ref().map(|s| (s.width, s.height)).unwrap_or((1920, 1080))
    }

    pub fn set_params(&self, scale: f64, fps: f64, bitrate: u32) {
        *self.params.lock().unwrap() = Params { scale: scale.clamp(1.0, 8.0), fps: fps.clamp(1.0, 60.0), bitrate: bitrate.clamp(100_000, 50_000_000) };
    }

    pub fn set_cursor_in_picture(&self, on: bool) {
        self.cursor.store(on, Ordering::Relaxed);
    }

    pub fn cursor_in_picture(&self) -> bool {
        self.cursor.load(Ordering::Relaxed)
    }

    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Encoder statistics in RTCOutboundRtpStreamStats terms; the limitation reason feeds
    /// the renderer's "auto" quality loop.
    pub fn stats(&self) -> Value {
        let mut s = self.stats.lock().unwrap();
        let now = Instant::now();
        let two = Duration::from_secs(2);
        s.window.retain(|t| now.duration_since(*t) < two);
        s.skips.retain(|t| now.duration_since(*t) < two);
        s.slows.retain(|t| now.duration_since(*t) < two);
        s.fast.retain(|t| now.duration_since(*t) < two);
        s.timed.retain(|t| now.duration_since(*t) < two);
        // share of frames with room to spare (1 when nothing is timed: hardware encoders)
        let fast_share = if s.timed.is_empty() { 1.0 } else { s.fast.len() as f64 / s.timed.len() as f64 };
        let recent = |t: Option<Instant>| t.is_some_and(|t| now.duration_since(t) < two);
        // Limited only when it keeps happening: a fifth or more of the frames in the last
        // two seconds (a Wi-Fi hiccup skips a frame or two and must not cost resolution).
        let (n, k, m) = (s.window.len(), s.skips.len(), s.slows.len());
        let reason = if m >= 3 && m * 5 >= n.max(1) {
            "cpu"
        } else if recent(s.last_drop) || (k >= 3 && k * 5 >= n + k) {
            "bandwidth"
        } else {
            "none"
        };
        json!({
            "framesEncoded": s.frames, "frameWidth": s.width, "frameHeight": s.height,
            "framesPerSecond": s.window.len() as f64 / 2.0, "encoderImplementation": s.implementation,
            "powerEfficientEncoder": s.hardware, "qualityLimitationReason": reason, "fastShare": fast_share,
        })
    }

    pub fn start(self: &Arc<Self>, sink: Arc<dyn Sink>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("encode".into())
            .spawn(move || {
                // macOS: ScreenCaptureKit → VideoToolbox in hardware. If it won't start
                // (busy, permission), software for a few seconds, then hardware again —
                // never the slow path for a whole session.
                #[cfg(target_os = "macos")]
                while std::env::var_os("MANYDESK_SOFTWARE_CAPTURE").is_none() && std::env::var_os("MANYDESK_SYNTH").is_none() {
                    match super::mac::run(me.clone(), sink.clone()) {
                        Ok(()) => return,
                        Err(e) => eprintln!("[mac capture] {e}; OpenH264 for now, hardware again shortly"),
                    }
                    if me.run(sink.clone(), Some(Instant::now() + Duration::from_secs(8))) {
                        return;
                    }
                }
                #[allow(unreachable_code)]
                {
                    me.run(sink, None);
                }
            })
            .ok();
    }

    // ---- for the platform pipelines (media/mac.rs) ----

    pub(crate) fn params(&self) -> Params {
        *self.params.lock().unwrap()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn take_keyframe(&self) -> bool {
        self.keyframe.swap(false, Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn keyframe_pending(&self) -> bool {
        self.keyframe.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }

    /// One encoded frame handed to the sink (ok) or dropped by it.
    /// A frame not encoded because the path was backed up: counts as bandwidth-limited for
    /// the quality loop, without forcing a keyframe.
    /// Software path: how long a frame took against its slot.
    fn timing(&self, took: Duration, slot: Duration) {
        let mut st = self.stats.lock().unwrap();
        let now = Instant::now();
        st.timed.push(now);
        if took < slot.mul_f64(0.6) {
            st.fast.push(now);
        }
    }

    pub(crate) fn skipped(&self) {
        crate::perf::tick("host-skipped");
        self.stats.lock().unwrap().skips.push(Instant::now());
    }

    pub(crate) fn record(&self, ok: bool, width: usize, height: usize, slow: bool, implementation: &'static str, hardware: bool) {
        crate::perf::tick(if ok { "host-encoded" } else { "host-sinkfull" });
        let mut st = self.stats.lock().unwrap();
        st.implementation = implementation;
        st.hardware = hardware;
        if slow {
            st.slows.push(Instant::now());
        }
        if ok {
            st.frames += 1;
            st.width = width;
            st.height = height;
            st.window.push(Instant::now());
        } else {
            st.last_drop = Some(Instant::now());
            self.keyframe.store(true, Ordering::Relaxed);
        }
    }

    /// The portable path (capture → I420 on the CPU → encoder). Until `until` if given;
    /// true when the video is over (stopped or the sink closed).
    fn run(self: &Arc<Self>, sink: Arc<dyn Sink>, until: Option<Instant>) -> bool {
        let mut src = match Source::open(self.screen.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[capture] {e}");
                // nothing to capture with: wait out the slot before trying again
                if let Some(u) = until {
                    std::thread::sleep(u.saturating_duration_since(Instant::now()));
                    return self.stop.load(Ordering::Relaxed) || sink.closed();
                }
                return true;
            }
        };
        let mut enc: Option<(H264, Params, usize, usize)> = None;
        #[cfg(windows)]
        let mut try_hardware = true;
        let mut prev: Vec<u8> = Vec::new();
        let mut yuv: Vec<u8> = Vec::new();
        // Refinement: once the screen has been still for a moment, the unchanged picture goes
        // through the encoder again — each pass codes what the earlier ones lost (crisp text).
        let mut last_change = Instant::now();
        let mut refined = REFINE_PASSES;
        while !self.stop.load(Ordering::Relaxed) && !sink.closed() {
            if until.is_some_and(|u| Instant::now() > u) {
                return false;
            }
            let p = *self.params.lock().unwrap();
            let interval = Duration::from_secs_f64(1.0 / p.fps);
            let t0 = Instant::now();
            if sink.congested() {
                self.skipped();
                std::thread::sleep(interval);
                continue;
            }
            let force = self.keyframe.swap(false, Ordering::Relaxed);
            let Some(frame) = src.grab(interval) else {
                std::thread::sleep(interval);
                continue;
            };
            crate::perf::add("sw-grab", t0.elapsed());
            let tc = Instant::now();
            let changed = prev.as_slice() != frame.data;
            let refine = !changed && refined < REFINE_PASSES && last_change.elapsed() >= STILL;
            // Nothing new: nothing to send (a keyframe request still gets one).
            if !changed && !force && !refine {
                drop(frame);
                sleep_rest(t0, interval);
                continue;
            }
            if changed {
                prev.clear();
                prev.extend_from_slice(frame.data);
                last_change = Instant::now();
                refined = 0;
            } else if refine {
                refined += 1;
            }
            crate::perf::add("sw-compare", tc.elapsed());
            let dw = ((frame.width as f64 / p.scale) as usize).max(16) & !1;
            let dh = ((frame.height as f64 / p.scale) as usize).max(16) & !1;
            // Bitrate alone changes in place on the hardware encoder.
            #[cfg(windows)]
            if let Some((H264::Mf(m), ep, w, h)) = &mut enc {
                if *w == dw && *h == dh && ep.fps == p.fps && ep.scale == p.scale && ep.bitrate != p.bitrate {
                    m.set_bitrate(p.bitrate);
                    ep.bitrate = p.bitrate;
                }
            }
            if !matches!(&enc, Some((_, ep, w, h)) if *ep == p && *w == dw && *h == dh) {
                enc = None;
                #[cfg(windows)]
                if try_hardware {
                    match super::win::MfEncoder::new(dw, dh, p.fps, p.bitrate) {
                        Ok(m) => {
                            eprintln!("[encode] Media Foundation: {}", m.name);
                            enc = Some((H264::Mf(m), p, dw, dh));
                        }
                        Err(e) => {
                            eprintln!("[encode] no hardware encoder ({e}); OpenH264");
                            try_hardware = false;
                        }
                    }
                }
            }
            if enc.is_none() {
                let cfg = EncoderConfig::new()
                    .bitrate(BitRate::from_bps(p.bitrate))
                    .max_frame_rate(FrameRate::from_hz(p.fps as f32))
                    .usage_type(UsageType::ScreenContentRealTime)
                    .rate_control_mode(RateControlMode::Bitrate)
                    .skip_frames(true)
                    .intra_frame_period(IntraFramePeriod::from_num_frames((p.fps * KEY_EVERY_SECS) as u32));
                match Encoder::with_api_config(OpenH264API::from_source(), cfg) {
                    Ok(e) => enc = Some((H264::Open(e), p, dw, dh)),
                    Err(e) => {
                        eprintln!("[encode] {e}");
                        return true;
                    }
                }
            }
            let ty = Instant::now();
            capture::to_i420(&frame, dw, dh, &mut yuv);
            crate::perf::add("sw-i420", ty.elapsed());
            drop(frame);
            let te = Instant::now();
            let (encoder, ..) = enc.as_mut().unwrap();
            let (frames, implementation, hardware): (Vec<(Vec<u8>, bool)>, &'static str, bool) = match encoder {
                H264::Open(e) => {
                    if force {
                        e.force_intra_frame();
                    }
                    let (cw, ch) = (dw / 2, dh / 2);
                    let (y, uv) = yuv.split_at(dw * dh);
                    let (u, v) = uv.split_at(cw * ch);
                    match e.encode(&YUVSlices::new((y, u, v), (dw, dh), (dw, cw, cw))) {
                        Ok(bs) => (vec![(bs.to_vec(), matches!(bs.frame_type(), FrameType::IDR | FrameType::I))], "OpenH264", false),
                        Err(_) => continue,
                    }
                }
                #[cfg(windows)]
                H264::Mf(m) => match m.encode(&yuv, force) {
                    Ok(f) => (f, "Media Foundation", true),
                    Err(e) => {
                        eprintln!("[encode] Media Foundation failed ({e}); OpenH264");
                        try_hardware = false;
                        enc = None;
                        continue;
                    }
                },
            };
            crate::perf::add("sw-encode", te.elapsed());
            // Test hook: MANYDESK_SIM_CPU=<ms per megapixel> — a slow machine (a VM without a
            // GPU encoder): each frame costs that much more, in proportion to its pixels.
            if let Some(ms) = std::env::var("MANYDESK_SIM_CPU").ok().and_then(|v| v.parse::<f64>().ok()) {
                std::thread::sleep(Duration::from_secs_f64(ms / 1000.0 * (dw * dh) as f64 / 1e6));
            }
            for (data, key) in frames {
                if data.is_empty() {
                    continue;
                }
                let ok = sink.push(EncodedFrame { data: Bytes::from(data), key, duration: interval });
                // "slow" only well past its slot: a little over is fine, and counting it would
                // shrink the picture further than the frame rate needs
                self.record(ok, dw, dh, t0.elapsed() > interval.mul_f64(1.5), implementation, hardware);
                self.timing(t0.elapsed(), interval);
            }
            sleep_rest(t0, interval);
        }
        true
    }
}

/// The screen counts as still after this long without a change.
/// A keyframe at least this often (seconds), whatever happens: lost frames bring one on
/// request, this one heals what nobody noticed (a decoder that hid an error, a frame lost
/// on the way to it) — the picture is never wrong for longer. A full-screen keyframe of
/// text is ~2 MB, so not much more often.
pub const KEY_EVERY_SECS: f64 = 30.0;

const STILL: Duration = Duration::from_millis(300);
/// Encoder passes over a still picture (software/Media Foundation path).
const REFINE_PASSES: u8 = 2;

fn sleep_rest(t0: Instant, interval: Duration) {
    let spent = t0.elapsed();
    if spent < interval {
        std::thread::sleep(interval - spent);
    }
}
