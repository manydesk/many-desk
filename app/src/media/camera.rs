// Session camera (AnyDesk's video call next to the remote screen): the local webcam →
// I420 → OpenH264 at a small size → a sink (the MDP link, see Session::send_camera).
// Either side of a session can turn its own camera on; the other one decodes it with
// the usual video decoder and shows it in a floating picture-in-picture.
//
//   macOS: AVFoundation (AVCaptureSession, BGRA)      Windows: Media Foundation source reader
//   Android: Camera2 in CameraCapture.java (I420)     elsewhere: no camera
// Test hook: MANYDESK_SYNTH_CAMERA=1 — a moving test picture instead of the real camera.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;

use super::capture::{self, Frame};
use super::encode::EncodedFrame;

/// The picture is sent no bigger than this (a face in a corner, not a second screen).
const MAX_W: usize = 640;
const MAX_H: usize = 480;
const FPS: f64 = 15.0;
const BITRATE: u32 = 500_000;
/// A keyframe every 2 s: a lost frame heals by itself even if the request gets lost too.
const KEY_EVERY: u32 = 30;

#[allow(dead_code)] // which formats a platform delivers
pub(crate) enum Format {
    Bgra { stride: usize },
    I420,
}

pub(crate) struct Raw {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub format: Format,
}

/// The newest picture from the platform camera, and why it stopped (if it did).
#[derive(Default)]
pub(crate) struct Slot {
    latest: Mutex<(u64, Option<Raw>)>,
    cond: Condvar,
    error: Mutex<Option<String>>,
}

impl Slot {
    pub fn put(&self, raw: Raw) {
        let mut l = self.latest.lock().unwrap();
        l.0 += 1;
        l.1 = Some(raw);
        self.cond.notify_all();
    }

    #[allow(dead_code)]
    pub fn fail(&self, e: String) {
        *self.error.lock().unwrap() = Some(e);
        self.cond.notify_all();
    }

    fn take(&self, seen: &mut u64, wait: Duration) -> Option<Raw> {
        let l = self.latest.lock().unwrap();
        let (mut l, _) = self.cond.wait_timeout_while(l, wait, |l| l.0 == *seen && self.error.lock().unwrap().is_none()).unwrap();
        if l.0 == *seen {
            return None;
        }
        *seen = l.0;
        l.1.take()
    }
}

/// The camera while it is on; dropping it turns it off.
pub struct Camera {
    stop: Arc<AtomicBool>,
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Camera {
    /// Turns the camera on. Frames go to `out`; `keyframe` set asks for a keyframe (the
    /// other side lost one); `failed` is called once if the camera can't run (the reason is
    /// "denied", "nocamera" or a system message).
    pub fn start(keyframe: Arc<AtomicBool>, out: impl Fn(EncodedFrame) + Send + 'static, failed: impl FnOnce(String) + Send + 'static) -> Camera {
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || {
                if let Err(e) = run(&st, &keyframe, &out) {
                    if !st.load(Ordering::SeqCst) {
                        eprintln!("[camera] {e}");
                        failed(e);
                    }
                }
            })
            .ok();
        Camera { stop }
    }
}

fn run(stop: &AtomicBool, keyframe: &AtomicBool, out: &dyn Fn(EncodedFrame)) -> Result<(), String> {
    let slot = Arc::new(Slot::default());
    let synth = std::env::var_os("MANYDESK_SYNTH_CAMERA").is_some();
    let _source = if synth { None } else { Some(platform::open(slot.clone())?) };
    let interval = Duration::from_secs_f64(1.0 / FPS);
    let mut seen = 0u64;
    let mut last_sent: Option<Instant> = None;
    let mut last_frame = Instant::now();
    let mut enc: Option<(Encoder, usize, usize)> = None;
    let mut yuv: Vec<u8> = Vec::new();
    let mut n: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        if let Some(e) = slot.error.lock().unwrap().take() {
            return Err(e);
        }
        let raw = if synth {
            std::thread::sleep(interval);
            Some(synth_frame(n))
        } else {
            slot.take(&mut seen, Duration::from_millis(500))
        };
        let Some(raw) = raw else {
            if last_frame.elapsed() > Duration::from_secs(10) {
                return Err("nocamera".into());
            }
            continue;
        };
        last_frame = Instant::now();
        n += 1;
        // the camera may run faster than we send
        if last_sent.is_some_and(|t| t.elapsed() < interval.mul_f64(0.8)) {
            continue;
        }
        last_sent = Some(Instant::now());
        let (dw, dh) = fit(raw.width, raw.height);
        match raw.format {
            Format::Bgra { stride } => {
                capture::to_i420(&Frame { data: &raw.data, width: raw.width, height: raw.height, stride, bgra: true }, dw, dh, &mut yuv);
            }
            Format::I420 if raw.width == dw && raw.height == dh => {
                yuv.clear();
                yuv.extend_from_slice(&raw.data);
            }
            // an I420 picture bigger than the send size: every other pixel (cameras deliver
            // at most 2× what is asked for)
            Format::I420 => i420_half(&raw, dw, dh, &mut yuv),
        }
        if !matches!(&enc, Some((_, w, h)) if *w == dw && *h == dh) {
            let cfg = EncoderConfig::new()
                .bitrate(BitRate::from_bps(BITRATE))
                .max_frame_rate(FrameRate::from_hz(FPS as f32))
                .usage_type(UsageType::CameraVideoRealTime)
                .rate_control_mode(RateControlMode::Bitrate)
                .skip_frames(true)
                .intra_frame_period(IntraFramePeriod::from_num_frames(KEY_EVERY));
            enc = Some((Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| e.to_string())?, dw, dh));
        }
        let (e, ..) = enc.as_mut().unwrap();
        if keyframe.swap(false, Ordering::Relaxed) {
            e.force_intra_frame();
        }
        let (cw, ch) = (dw / 2, dh / 2);
        let (y, uv) = yuv.split_at(dw * dh);
        let (u, v) = uv.split_at(cw * ch);
        let Ok(bs) = e.encode(&YUVSlices::new((y, u, v), (dw, dh), (dw, cw, cw))) else { continue };
        let key = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
        let data = bs.to_vec();
        if !data.is_empty() {
            out(EncodedFrame { data: Bytes::from(data), key, duration: interval });
        }
    }
    Ok(())
}

/// The send size for a `w`×`h` camera picture: within MAX_W×MAX_H, aspect kept, even.
fn fit(w: usize, h: usize) -> (usize, usize) {
    let s = (MAX_W as f64 / w as f64).min(MAX_H as f64 / h as f64).min(1.0);
    (((w as f64 * s) as usize & !1).max(16), ((h as f64 * s) as usize & !1).max(16))
}

/// I420 → I420 at `dw`×`dh` by picking pixels (nearest neighbour).
fn i420_half(raw: &Raw, dw: usize, dh: usize, out: &mut Vec<u8>) {
    let (sw, sh) = (raw.width, raw.height);
    let (scw, sch) = (sw / 2, sh / 2);
    let (dcw, dch) = (dw / 2, dh / 2);
    out.clear();
    let (y, uv) = raw.data.split_at(sw * sh);
    let (u, v) = uv.split_at(scw * sch);
    for r in 0..dh {
        let sr = r * sh / dh;
        out.extend((0..dw).map(|c| y[sr * sw + c * sw / dw]));
    }
    for plane in [u, v] {
        for r in 0..dch {
            let sr = r * sch / dch;
            out.extend((0..dcw).map(|c| plane[sr * scw + c * scw / dcw]));
        }
    }
}

/// Rotates an I420 picture by 90, 180 or 270 degrees clockwise (phones: the sensor is
/// mounted sideways). Returns the new width and height.
#[allow(dead_code)]
pub(crate) fn i420_rotate(src: &[u8], w: usize, h: usize, deg: u32) -> (Vec<u8>, usize, usize) {
    let deg = deg % 360;
    if deg == 0 {
        return (src.to_vec(), w, h);
    }
    let (cw, ch) = (w / 2, h / 2);
    let (y, uv) = src.split_at(w * h);
    let (u, v) = uv.split_at(cw * ch);
    let (nw, nh) = if deg == 180 { (w, h) } else { (h, w) };
    let mut out = Vec::with_capacity(src.len());
    let rot = |p: &[u8], pw: usize, ph: usize, out: &mut Vec<u8>| {
        let (ow, oh) = if deg == 180 { (pw, ph) } else { (ph, pw) };
        for r in 0..oh {
            for c in 0..ow {
                let (sx, sy) = match deg {
                    90 => (r, ph - 1 - c),
                    180 => (pw - 1 - c, ph - 1 - r),
                    _ => (pw - 1 - r, c),
                };
                out.push(p[sy * pw + sx]);
            }
        }
    };
    rot(y, w, h, &mut out);
    rot(u, cw, ch, &mut out);
    rot(v, cw, ch, &mut out);
    (out, nw, nh)
}

/// Test picture: colour bars sliding sideways and a white square going round.
fn synth_frame(n: u64) -> Raw {
    let (w, h) = (640usize, 480usize);
    let mut data = vec![0u8; w * h * 4];
    let bars: [[u8; 3]; 7] = [[255, 255, 255], [0, 255, 255], [255, 255, 0], [0, 255, 0], [255, 0, 255], [0, 0, 255], [255, 0, 0]];
    let shift = (n as usize * 6) % w;
    let a = n as f64 / 15.0;
    let (sx, sy) = ((w as f64 / 2.0 + a.cos() * 180.0) as usize, (h as f64 / 2.0 + a.sin() * 140.0) as usize);
    for yy in 0..h {
        for xx in 0..w {
            let b = bars[((xx + shift) % w) * 7 / w];
            let i = (yy * w + xx) * 4;
            let sq = xx.abs_diff(sx) < 40 && yy.abs_diff(sy) < 40;
            let c = if sq { [255, 255, 255] } else { b };
            data[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
    Raw { data, width: w, height: h, format: Format::Bgra { stride: w * 4 } }
}

// ------------------------------------------------------------------- platforms ---

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::Arc;
    use std::time::Duration;

    use block2::RcBlock;
    use dispatch2::{DispatchQueue, DispatchQueueAttr};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
    use objc2_core_media::CMSampleBuffer;
    use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

    use super::{Format, Raw, Slot};

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeVideo: &'static NSString;
        static AVCaptureSessionPreset640x480: &'static NSString;
    }

    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        fn CMSampleBufferGetImageBuffer(sb: *const c_void) -> *mut c_void;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: &'static NSString;
        fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *const u8;
        fn CVPixelBufferGetWidth(pb: *const c_void) -> usize;
        fn CVPixelBufferGetHeight(pb: *const c_void) -> usize;
        fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
    }

    const BGRA: u32 = 0x4247_5241; // kCVPixelFormatType_32BGRA

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "ManyDeskCameraOutput"]
        #[ivars = Arc<Slot>]
        struct CameraOutput;

        unsafe impl NSObjectProtocol for CameraOutput {}

        impl CameraOutput {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            unsafe fn did_output(&self, _out: &AnyObject, sample: &CMSampleBuffer, _conn: &AnyObject) {
                self.frame(sample as *const CMSampleBuffer as *const c_void);
            }
        }
    );

    impl CameraOutput {
        fn new(slot: Arc<Slot>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(slot);
            unsafe { msg_send![super(this), init] }
        }

        fn frame(&self, sb: *const c_void) {
            unsafe {
                let img = CMSampleBufferGetImageBuffer(sb);
                if img.is_null() {
                    return;
                }
                CVPixelBufferLockBaseAddress(img, 1);
                let (w, h, stride, base) = (CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img), CVPixelBufferGetBytesPerRow(img), CVPixelBufferGetBaseAddress(img));
                if !base.is_null() && w > 0 && h > 0 {
                    let data = std::slice::from_raw_parts(base, stride * h).to_vec();
                    self.ivars().put(Raw { data, width: w, height: h, format: Format::Bgra { stride } });
                }
                CVPixelBufferUnlockBaseAddress(img, 1);
            }
        }
    }

    /// The running capture session; stopped when dropped.
    pub struct Source {
        session: Retained<AnyObject>,
        _output: Retained<CameraOutput>,
    }

    impl Drop for Source {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.session, stopRunning];
            }
        }
    }

    /// Asks for the camera (the system prompt, the first time) and waits for the answer.
    fn access() -> Result<(), String> {
        let cls = AnyClass::get(c"AVCaptureDevice").ok_or("no AVFoundation")?;
        let status: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: AVMediaTypeVideo] };
        match status {
            3 => Ok(()),
            0 => {
                let (tx, rx) = std::sync::mpsc::channel::<bool>();
                let block = RcBlock::new(move |granted: Bool| {
                    let _ = tx.send(granted.as_bool());
                });
                unsafe {
                    let _: () = msg_send![cls, requestAccessForMediaType: AVMediaTypeVideo, completionHandler: &*block];
                }
                match rx.recv_timeout(Duration::from_secs(120)) {
                    Ok(true) => Ok(()),
                    _ => Err("denied".into()),
                }
            }
            _ => Err("denied".into()),
        }
    }

    pub fn open(slot: Arc<Slot>) -> Result<Source, String> {
        access()?;
        unsafe {
            let dev_cls = AnyClass::get(c"AVCaptureDevice").ok_or("no AVFoundation")?;
            let dev: Option<Retained<AnyObject>> = msg_send![dev_cls, defaultDeviceWithMediaType: AVMediaTypeVideo];
            let dev = dev.ok_or("nocamera")?;
            let input_cls = AnyClass::get(c"AVCaptureDeviceInput").ok_or("no AVFoundation")?;
            let mut err: *mut AnyObject = std::ptr::null_mut();
            let input: Option<Retained<AnyObject>> = msg_send![input_cls, deviceInputWithDevice: &*dev, error: &mut err];
            let input = input.ok_or_else(|| {
                if err.is_null() {
                    "camera busy".to_string()
                } else {
                    let d: Retained<NSString> = msg_send![&*err, localizedDescription];
                    d.to_string()
                }
            })?;
            let session: Retained<AnyObject> = msg_send![AnyClass::get(c"AVCaptureSession").ok_or("no AVFoundation")?, new];
            let _: () = msg_send![&*session, beginConfiguration];
            let ok: bool = msg_send![&*session, canSetSessionPreset: AVCaptureSessionPreset640x480];
            if ok {
                let _: () = msg_send![&*session, setSessionPreset: AVCaptureSessionPreset640x480];
            }
            let ok: bool = msg_send![&*session, canAddInput: &*input];
            if !ok {
                return Err("camera busy".into());
            }
            let _: () = msg_send![&*session, addInput: &*input];
            let output: Retained<AnyObject> = msg_send![AnyClass::get(c"AVCaptureVideoDataOutput").ok_or("no AVFoundation")?, new];
            let fmt = NSNumber::new_u32(BGRA);
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[kCVPixelBufferPixelFormatTypeKey], &[&*fmt as &AnyObject]);
            let _: () = msg_send![&*output, setVideoSettings: &*settings];
            let _: () = msg_send![&*output, setAlwaysDiscardsLateVideoFrames: true];
            let delegate = CameraOutput::new(slot);
            let queue = DispatchQueue::new("manydesk.camera", DispatchQueueAttr::SERIAL);
            let _: () = msg_send![&*output, setSampleBufferDelegate: &*delegate, queue: &*queue];
            let ok: bool = msg_send![&*session, canAddOutput: &*output];
            if !ok {
                return Err("camera busy".into());
            }
            let _: () = msg_send![&*session, addOutput: &*output];
            let _: () = msg_send![&*session, commitConfiguration];
            let _: () = msg_send![&*session, startRunning];
            Ok(Source { session, _output: delegate })
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use windows::core::Interface;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    use super::{Format, Raw, Slot};

    /// The reader thread; stopped when dropped.
    pub struct Source {
        stop: Arc<AtomicBool>,
    }

    impl Drop for Source {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    fn reader() -> windows::core::Result<Option<IMFSourceReader>> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let mut attr: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attr, 1)?;
            let attr = attr.unwrap();
            attr.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attr, &mut list, &mut count)?;
            if count == 0 || list.is_null() {
                return Ok(None);
            }
            let acts = std::slice::from_raw_parts_mut(list, count as usize);
            let first = acts[0].take();
            for a in acts.iter_mut() {
                drop(a.take());
            }
            CoTaskMemFree(Some(list as *const _));
            let Some(act) = first else { return Ok(None) };
            let source: IMFMediaSource = act.ActivateObject()?;
            let mut ra: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut ra, 1)?;
            let ra = ra.unwrap();
            ra.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &ra)?;
            // the camera's own mode closest to 640×480 (less to convert and scale)
            let mut best: Option<(u64, IMFMediaType)> = None;
            for i in 0..200 {
                let Ok(t) = reader.GetNativeMediaType(STREAM, i) else { break };
                let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) else { continue };
                let (w, h) = ((size >> 32) as i64, (size & 0xffff_ffff) as i64);
                let score = ((w - 640).abs() + (h - 480).abs()) as u64;
                if best.as_ref().is_none_or(|b| score < b.0) {
                    best = Some((score, t));
                }
            }
            if let Some((_, t)) = best {
                let _ = reader.SetCurrentMediaType(STREAM, None, &t);
            }
            let mt = MFCreateMediaType()?;
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(STREAM, None, &mt)?;
            Ok(Some(reader))
        }
    }

    fn read(reader: &IMFSourceReader, slot: &Slot, stop: &AtomicBool) -> windows::core::Result<()> {
        unsafe {
            let cur = reader.GetCurrentMediaType(STREAM)?;
            let size = cur.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
            while !stop.load(Ordering::SeqCst) {
                let mut flags = 0u32;
                let mut sample: Option<IMFSample> = None;
                reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };
                let buf = sample.ConvertToContiguousBuffer()?;
                let mut data = vec![0u8; w * h * 4];
                if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                    // the pitch is negative for a bottom-up picture; row 0 is the top either way
                    let (mut p, mut pitch) = (std::ptr::null_mut::<u8>(), 0i32);
                    b2.Lock2D(&mut p, &mut pitch)?;
                    for r in 0..h {
                        let row = p.offset(r as isize * pitch as isize);
                        std::ptr::copy_nonoverlapping(row, data[r * w * 4..].as_mut_ptr(), w * 4);
                    }
                    let _ = b2.Unlock2D();
                } else {
                    let (mut p, mut len) = (std::ptr::null_mut::<u8>(), 0u32);
                    buf.Lock(&mut p, None, Some(&mut len))?;
                    let n = (len as usize).min(data.len());
                    std::ptr::copy_nonoverlapping(p, data.as_mut_ptr(), n);
                    let _ = buf.Unlock();
                }
                slot.put(Raw { data, width: w, height: h, format: Format::Bgra { stride: w * 4 } });
            }
            Ok(())
        }
    }

    pub fn open(slot: Arc<Slot>) -> Result<Source, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::Builder::new()
            .name("camera-read".into())
            .spawn(move || {
                let reader = match reader() {
                    Ok(Some(r)) => r,
                    Ok(None) => return drop(tx.send(Err("nocamera".into()))),
                    // E_ACCESSDENIED: Settings → Privacy → Camera is off for desktop apps
                    Err(e) if e.code().0 == 0x8007_0005_u32 as i32 => return drop(tx.send(Err("denied".into()))),
                    Err(e) => return drop(tx.send(Err(e.message()))),
                };
                let _ = tx.send(Ok(()));
                if let Err(e) = read(&reader, &slot, &st) {
                    if !st.load(Ordering::SeqCst) {
                        slot.fail(e.message());
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|_| "camera".to_string())??;
        Ok(Source { stop })
    }
}

#[cfg(target_os = "android")]
mod platform {
    use std::sync::{Arc, Mutex};

    use jni::objects::{JByteArray, JClass, JString};
    use jni::sys::jint;
    use jni::JNIEnv;

    use super::{Format, Raw, Slot};

    /// Where CameraCapture.java's pictures go while the camera is on.
    static SLOT: Mutex<Option<Arc<Slot>>> = Mutex::new(None);

    pub struct Source;

    impl Drop for Source {
        fn drop(&mut self) {
            *SLOT.lock().unwrap() = None;
            crate::android::camera(false);
        }
    }

    pub fn open(slot: Arc<Slot>) -> Result<Source, String> {
        *SLOT.lock().unwrap() = Some(slot);
        crate::android::camera(true);
        Ok(Source)
    }

    /// One picture: I420, `rot` degrees clockwise from upright.
    #[no_mangle]
    pub extern "system" fn Java_az_manydesk_CameraCapture_nativeFrame(env: JNIEnv, _c: JClass, data: JByteArray, w: jint, h: jint, rot: jint) {
        let Some(slot) = SLOT.lock().unwrap().clone() else { return };
        let (w, h) = (w.max(0) as usize & !1, h.max(0) as usize & !1);
        let Ok(bytes) = env.convert_byte_array(&data) else { return };
        if w == 0 || h == 0 || bytes.len() < w * h * 3 / 2 {
            return;
        }
        let (data, width, height) = super::i420_rotate(&bytes[..w * h * 3 / 2], w, h, rot.max(0) as u32);
        slot.put(Raw { data, width, height, format: Format::I420 });
    }

    /// The camera could not be opened ("denied", "nocamera" or Android's message).
    #[no_mangle]
    pub extern "system" fn Java_az_manydesk_CameraCapture_nativeError(mut env: JNIEnv, _c: JClass, e: JString) {
        let e: String = env.get_string(&e).map(|s| s.into()).unwrap_or_default();
        if let Some(slot) = SLOT.lock().unwrap().clone() {
            slot.fail(e);
        }
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "android")))]
mod platform {
    use std::sync::Arc;

    use super::Slot;

    pub struct Source;

    pub fn open(_slot: Arc<Slot>) -> Result<Source, String> {
        Err("nocamera".into())
    }
}
