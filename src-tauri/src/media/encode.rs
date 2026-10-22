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
    last_drop: Option<Instant>,
    last_slow: Option<Instant>,
    implementation: &'static str,
    hardware: bool,
}

pub struct Video {
    screen: Option<Screen>,
    params: Mutex<Params>,
    keyframe: AtomicBool,
    stop: AtomicBool,
    started: AtomicBool,
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
            stats: Default::default(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.screen.as_ref().map(|s| (s.width, s.height)).unwrap_or((1920, 1080))
    }

    pub fn set_params(&self, scale: f64, fps: f64, bitrate: u32) {
        *self.params.lock().unwrap() = Params { scale: scale.clamp(1.0, 8.0), fps: fps.clamp(1.0, 60.0), bitrate: bitrate.clamp(100_000, 50_000_000) };
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
        s.window.retain(|t| now.duration_since(*t) < Duration::from_secs(2));
        let recent = |t: Option<Instant>| t.is_some_and(|t| now.duration_since(t) < Duration::from_secs(2));
        let reason = if recent(s.last_slow) { "cpu" } else if recent(s.last_drop) { "bandwidth" } else { "none" };
        json!({
            "framesEncoded": s.frames, "frameWidth": s.width, "frameHeight": s.height,
            "framesPerSecond": s.window.len() as f64 / 2.0, "encoderImplementation": s.implementation,
            "powerEfficientEncoder": s.hardware, "qualityLimitationReason": reason,
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
                // macOS: ScreenCaptureKit → VideoToolbox in hardware; OpenH264 if that fails.
                #[cfg(target_os = "macos")]
                match super::mac::run(me.clone(), sink.clone()) {
                    Ok(()) => return,
                    Err(e) => eprintln!("[mac capture] {e}; falling back to OpenH264"),
                }
                me.run(sink)
            })
            .ok();
    }

    // ---- for the platform pipelines (media/mac.rs) ----

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn params(&self) -> Params {
        *self.params.lock().unwrap()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn take_keyframe(&self) -> bool {
        self.keyframe.swap(false, Ordering::Relaxed)
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
    pub(crate) fn record(&self, ok: bool, width: usize, height: usize, slow: bool, implementation: &'static str, hardware: bool) {
        let mut st = self.stats.lock().unwrap();
        st.implementation = implementation;
        st.hardware = hardware;
        if slow {
            st.last_slow = Some(Instant::now());
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

    fn run(self: Arc<Self>, sink: Arc<dyn Sink>) {
        let mut src = match Source::open(self.screen.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[capture] {e}");
                return;
            }
        };
        let mut enc: Option<(H264, Params, usize, usize)> = None;
        #[cfg(windows)]
        let mut try_hardware = true;
        let mut prev: Vec<u8> = Vec::new();
        let mut yuv: Vec<u8> = Vec::new();
        let mut last_sent = Instant::now() - Duration::from_secs(10);
        while !self.stop.load(Ordering::Relaxed) && !sink.closed() {
            let p = *self.params.lock().unwrap();
            let interval = Duration::from_secs_f64(1.0 / p.fps);
            let t0 = Instant::now();
            let force = self.keyframe.swap(false, Ordering::Relaxed);
            let Some(frame) = src.grab(interval) else {
                std::thread::sleep(interval);
                continue;
            };
            let changed = prev.as_slice() != frame.data;
            if !changed && !force && last_sent.elapsed() < Duration::from_secs(1) {
                drop(frame);
                sleep_rest(t0, interval);
                continue;
            }
            if changed {
                prev.clear();
                prev.extend_from_slice(frame.data);
            }
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
                    .intra_frame_period(IntraFramePeriod::from_num_frames((p.fps * 10.0) as u32));
                match Encoder::with_api_config(OpenH264API::from_source(), cfg) {
                    Ok(e) => enc = Some((H264::Open(e), p, dw, dh)),
                    Err(e) => {
                        eprintln!("[encode] {e}");
                        return;
                    }
                }
            }
            capture::to_i420(&frame, dw, dh, &mut yuv);
            drop(frame);
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
            for (data, key) in frames {
                if data.is_empty() {
                    continue;
                }
                let ok = sink.push(EncodedFrame { data: Bytes::from(data), key, duration: interval });
                self.record(ok, dw, dh, t0.elapsed() > interval, implementation, hardware);
                if ok {
                    last_sent = Instant::now();
                }
            }
            sleep_rest(t0, interval);
        }
    }
}

fn sleep_rest(t0: Instant, interval: Duration) {
    let spent = t0.elapsed();
    if spent < interval {
        std::thread::sleep(interval - spent);
    }
}
