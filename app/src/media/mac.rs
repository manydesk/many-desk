// macOS host video: ScreenCaptureKit → VideoToolbox, all in hardware and zero-copy.
// ScreenCaptureKit scales on the GPU to the size the viewer can show and delivers NV12
// only when the screen changed; each IOSurface-backed frame goes straight into a
// low-latency hardware H.264 session. Output is converted AVCC → Annex-B for the sink.
use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use bytes::Bytes;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};

use super::encode::{EncodedFrame, Params, Sink, Video};

// ---- CoreFoundation / CoreMedia / CoreVideo / VideoToolbox / CoreGraphics (C API) ----

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFTypeArrayCallBacks: c_void;
    fn CFDictionaryCreateMutable(a: CFTypeRef, cap: isize, k: *const c_void, v: *const c_void) -> *mut c_void;
    fn CFDictionarySetValue(d: *mut c_void, k: CFTypeRef, v: CFTypeRef);
    fn CFDictionaryContainsKey(d: CFTypeRef, k: CFTypeRef) -> u8;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, t: isize, out: *mut c_void) -> u8;
    fn CFNumberCreate(a: CFTypeRef, t: isize, v: *const c_void) -> CFTypeRef;
    fn CFArrayCreate(a: CFTypeRef, v: *const CFTypeRef, n: isize, cb: *const c_void) -> CFTypeRef;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    static kCMSampleAttachmentKey_NotSync: CFStringRef;
    fn CMSampleBufferGetImageBuffer(sb: *const c_void) -> *mut c_void;
    fn CMSampleBufferGetDataBuffer(sb: *const c_void) -> *mut c_void;
    fn CMSampleBufferGetFormatDescription(sb: *const c_void) -> *const c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sb: *const c_void) -> CMTime;
    fn CMSampleBufferGetSampleAttachmentsArray(sb: *const c_void, create: u8) -> CFTypeRef;
    fn CMBlockBufferGetDataLength(bb: *const c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(bb: *const c_void, off: usize, len: usize, dst: *mut c_void) -> i32;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        d: *const c_void,
        i: usize,
        ptr: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_len: *mut i32,
    ) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetWidth(pb: *const c_void) -> usize;
    fn CVPixelBufferGetHeight(pb: *const c_void) -> usize;
    fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *const u8;
    fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    static kVTCompressionPropertyKey_MaxAllowedFrameQP: CFStringRef;
    static kVTCompressionPropertyKey_MinAllowedFrameQP: CFStringRef;
    static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    fn VTCompressionSessionCreate(
        a: CFTypeRef,
        w: i32,
        h: i32,
        codec: u32,
        spec: CFTypeRef,
        src: CFTypeRef,
        ca: CFTypeRef,
        cb: extern "C" fn(*mut c_void, *mut c_void, i32, u32, *const c_void),
        refcon: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn VTSessionSetProperty(s: *mut c_void, k: CFStringRef, v: CFTypeRef) -> i32;
    fn VTSessionCopyProperty(s: *mut c_void, k: CFStringRef, a: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn VTCompressionSessionPrepareToEncodeFrames(s: *mut c_void) -> i32;
    fn VTCompressionSessionEncodeFrame(s: *mut c_void, img: *mut c_void, pts: CMTime, dur: CMTime, props: CFTypeRef, rc: *mut c_void, flags: *mut u32) -> i32;
    fn VTCompressionSessionCompleteFrames(s: *mut c_void, until: CMTime) -> i32;
    fn VTCompressionSessionInvalidate(s: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayCopyDisplayMode(id: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(m: *mut c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(m: *mut c_void) -> usize;
    fn CGDisplayModeRelease(m: *mut c_void);
}

const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
const NV12: u32 = u32::from_be_bytes(*b"420v");
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const CF_SINT32: isize = 3;
const CF_FLOAT64: isize = 6;

fn cf_i32(v: i32) -> CFTypeRef {
    unsafe { CFNumberCreate(ptr::null(), CF_SINT32, &v as *const i32 as *const c_void) }
}

fn cf_f64(v: f64) -> CFTypeRef {
    unsafe { CFNumberCreate(ptr::null(), CF_FLOAT64, &v as *const f64 as *const c_void) }
}

fn set(s: *mut c_void, k: CFStringRef, v: CFTypeRef, owned: bool) {
    unsafe {
        VTSessionSetProperty(s, k, v);
        if owned {
            CFRelease(v);
        }
    }
}

fn cm_time(value: i64, timescale: i32) -> CMTime {
    CMTime { value, timescale, flags: CMTimeFlags::Valid, epoch: 0 }
}

// ---- the VideoToolbox session ----

/// Shared with the encoder's output callback (refcon).
struct Out {
    video: Arc<Video>,
    sink: Arc<dyn Sink>,
    size: Mutex<(usize, usize)>,
    hardware: std::sync::atomic::AtomicBool,
    /// the encoder dropped a refinement frame (its QP cap was out of reach): try again
    refine_dropped: std::sync::atomic::AtomicBool,
}

struct Encoder {
    session: *mut c_void,
    width: usize,
    height: usize,
    params: Option<Params>,
    /// The newest captured picture (retained CVPixelBuffer) and its time: re-encoded once
    /// the screen has been still for a moment (refinement), or for a keyframe request.
    last: *mut c_void,
    last_pts: CMTime,
    last_at: Instant,
    refined: bool,
    retries: u8,
}

/// The screen counts as still after this long without a new picture.
const STILL: Duration = Duration::from_millis(300);
/// Refinement frame QP (fixed; normal frames use rate control above `MIN_QP`): a still
/// screen of text comes out nearly lossless. A full screen of new text costs ~640 KB once
/// at 16, ~380 KB at 20 — the coarser one on slower links.
fn refine_qp(bitrate: u32) -> i32 {
    if bitrate >= 4_000_000 {
        16
    } else {
        20
    }
}
/// Normal frames are never finer than this: no bits on detail the refinement brings once
/// the screen stops (scrolling: about half the traffic; motion quality measured the same).
const MIN_QP: i32 = 24;

/// MIN_QP for a bitrate: with bits to spare (a fast path), moving content may come out
/// finer too — the cap only saves traffic where traffic is short.
fn min_qp(bitrate: u32) -> i32 {
    if bitrate >= 15_000_000 {
        18
    } else if bitrate >= 6_000_000 {
        22
    } else {
        MIN_QP
    }
}
/// sourceFrameRefcon of a refinement frame (for the perf log).
const REFINE_TAG: usize = 1;

unsafe impl Send for Encoder {}

impl Encoder {
    fn new() -> Self {
        Encoder { session: ptr::null_mut(), width: 0, height: 0, params: None, last: ptr::null_mut(), last_pts: cm_time(0, 0), last_at: Instant::now(), refined: true, retries: 0 }
    }

    fn keep(&mut self, img: *mut c_void, pts: CMTime) {
        unsafe {
            CFRetain(img as CFTypeRef);
            if !self.last.is_null() {
                CFRelease(self.last as CFTypeRef);
            }
        }
        self.last = img;
        self.last_pts = pts;
        self.last_at = Instant::now();
        self.refined = false;
        self.retries = 0;
    }

    /// One picture into the session; `refine`: a still screen's high-quality pass.
    unsafe fn encode(&mut self, img: *mut c_void, pts: CMTime, key: bool, refine: bool) -> i32 {
        let props = if key {
            let d = CFDictionaryCreateMutable(ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            CFDictionarySetValue(d, kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue);
            d as CFTypeRef
        } else {
            ptr::null()
        };
        if refine {
            // exactly this QP: finer would only spend bits on a picture already refined
            let qp = refine_qp(self.params.map(|p| p.bitrate).unwrap_or(0));
            set(self.session, kVTCompressionPropertyKey_MinAllowedFrameQP, cf_i32(qp), true);
            set(self.session, kVTCompressionPropertyKey_MaxAllowedFrameQP, cf_i32(qp), true);
        }
        let rc = if refine { REFINE_TAG } else { 0 } as *mut c_void;
        let st = VTCompressionSessionEncodeFrame(self.session, img, pts, cm_time(0, 0), props, rc, ptr::null_mut());
        if refine {
            // Only this frame: wait for it, then lift the cap again.
            VTCompressionSessionCompleteFrames(self.session, CMTime { value: 0, timescale: 0, flags: CMTimeFlags::empty(), epoch: 0 });
            set(self.session, kVTCompressionPropertyKey_MaxAllowedFrameQP, cf_i32(51), true);
            set(self.session, kVTCompressionPropertyKey_MinAllowedFrameQP, cf_i32(min_qp(self.params.map(|p| p.bitrate).unwrap_or(0))), true);
        }
        if !props.is_null() {
            CFRelease(props);
        }
        st
    }

    /// The screen has been still: encode the last picture once more at high quality
    /// (or as the keyframe the viewer asked for). Called from the capture loop.
    fn idle(&mut self, out: &Out) {
        let (video, sink) = (&*out.video, &*out.sink);
        if self.session.is_null() || self.last.is_null() || self.last_at.elapsed() < STILL || video.stopped() {
            return;
        }
        if out.refine_dropped.swap(false, std::sync::atomic::Ordering::Relaxed) && self.retries < 3 {
            self.retries += 1;
            self.refined = false;
        }
        let key = video.keyframe_pending();
        if self.refined && !key {
            return;
        }
        if sink.congested() {
            return;
        }
        let key = video.take_keyframe();
        // a little after the picture's own time, on its clock
        let t = self.last_pts;
        let pts = cm_time(t.value + (self.last_at.elapsed().as_secs_f64() * t.timescale as f64) as i64, t.timescale);
        self.last_pts = pts;
        let st = unsafe { self.encode(self.last, pts, key, !self.refined) };
        if st != 0 {
            eprintln!("[videotoolbox] refine failed: {st}");
        }
        self.refined = true;
    }

    fn close(&mut self) {
        if !self.last.is_null() {
            unsafe { CFRelease(self.last as CFTypeRef) };
            self.last = ptr::null_mut();
        }
        if !self.session.is_null() {
            unsafe {
                VTCompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            self.session = ptr::null_mut();
        }
    }

    fn open(&mut self, out: *const Out, w: usize, h: usize) -> Result<(), i32> {
        self.close();
        unsafe {
            // Best first: hardware with low-latency rate control (no frame buffering),
            // then plain hardware, then whatever VideoToolbox offers.
            let mut s: *mut c_void = ptr::null_mut();
            let mut st = -1;
            for (hw, low_latency) in [(true, true), (true, false), (false, false)] {
                let spec = CFDictionaryCreateMutable(ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
                if hw {
                    CFDictionarySetValue(spec, kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, kCFBooleanTrue);
                }
                if low_latency {
                    CFDictionarySetValue(spec, kVTVideoEncoderSpecification_EnableLowLatencyRateControl, kCFBooleanTrue);
                }
                st = VTCompressionSessionCreate(ptr::null(), w as i32, h as i32, CODEC_H264, spec, ptr::null(), ptr::null(), on_encoded, out as *mut c_void, &mut s);
                CFRelease(spec);
                if st == 0 && !s.is_null() {
                    (*out).hardware.store(hw, std::sync::atomic::Ordering::Relaxed);
                    eprintln!("[videotoolbox] hw={hw} low_latency={low_latency}");
                    break;
                }
            }
            if st != 0 || s.is_null() {
                return Err(st);
            }
            set(s, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue, false);
            set(s, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse, false);
            set(s, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel, false);
            // Lost frames bring a keyframe on request; the periodic one heals the rest
            // (see KEY_EVERY_SECS).
            set(s, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, cf_f64(crate::media::encode::KEY_EVERY_SECS as f64), true);
            set(s, kVTCompressionPropertyKey_MinAllowedFrameQP, cf_i32(MIN_QP), true);
            VTCompressionSessionPrepareToEncodeFrames(s);
            let mut hw: CFTypeRef = ptr::null();
            if VTSessionCopyProperty(s, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, ptr::null(), &mut hw) == 0 && !hw.is_null() {
                if hw == kCFBooleanTrue {
                    (*out).hardware.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                CFRelease(hw);
            }
            self.session = s;
            eprintln!("[videotoolbox] {w}x{h} hardware={}", (*out).hardware.load(std::sync::atomic::Ordering::Relaxed));
        }
        self.width = w;
        self.height = h;
        self.params = None;
        Ok(())
    }

    fn apply(&mut self, p: Params) {
        if self.params == Some(p) || self.session.is_null() {
            return;
        }
        let s = self.session;
        set(s, unsafe { kVTCompressionPropertyKey_AverageBitRate }, cf_i32(p.bitrate as i32), true);
        set(s, unsafe { kVTCompressionPropertyKey_MinAllowedFrameQP }, cf_i32(min_qp(p.bitrate)), true);
        set(s, unsafe { kVTCompressionPropertyKey_ExpectedFrameRate }, cf_f64(p.fps), true);
        // Hard cap: 1.5× the average over one second (bytes, seconds).
        unsafe {
            let bytes = cf_i32((p.bitrate as f64 * 1.5 / 8.0) as i32);
            let secs = cf_f64(1.0);
            let vals = [bytes, secs];
            let arr = CFArrayCreate(ptr::null(), vals.as_ptr(), 2, &kCFTypeArrayCallBacks);
            set(s, kVTCompressionPropertyKey_DataRateLimits, arr, true);
            CFRelease(bytes);
            CFRelease(secs);
        }
        self.params = Some(p);
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.close();
    }
}

extern "C" fn on_encoded(refcon: *mut c_void, src: *mut c_void, status: i32, flags: u32, sb: *const c_void) {
    if status != 0 || sb.is_null() || refcon.is_null() {
        if flags & 2 != 0 && src as usize == REFINE_TAG && !refcon.is_null() {
            crate::perf::tick("host-refine-dropped");
            unsafe { &*(refcon as *const Out) }.refine_dropped.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    let out = unsafe { &*(refcon as *const Out) };
    unsafe {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, 0);
        let key = attachments.is_null()
            || CFArrayGetCount(attachments) == 0
            || CFDictionaryContainsKey(CFArrayGetValueAtIndex(attachments, 0), kCMSampleAttachmentKey_NotSync) == 0;
        let mut data = Vec::new();
        if key {
            // SPS/PPS in front of every keyframe, so a decoder can start anywhere.
            let desc = CMSampleBufferGetFormatDescription(sb);
            let mut count = 0usize;
            let (mut p, mut n, mut hl) = (ptr::null(), 0usize, 0i32);
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, 0, &mut p, &mut n, &mut count, &mut hl);
            for i in 0..count {
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(desc, i, &mut p, &mut n, ptr::null_mut(), ptr::null_mut()) == 0 {
                    data.extend_from_slice(&[0, 0, 0, 1]);
                    data.extend_from_slice(std::slice::from_raw_parts(p, n));
                }
            }
        }
        let bb = CMSampleBufferGetDataBuffer(sb);
        if bb.is_null() {
            return;
        }
        let len = CMBlockBufferGetDataLength(bb);
        let mut avcc = vec![0u8; len];
        if CMBlockBufferCopyDataBytes(bb, 0, len, avcc.as_mut_ptr() as *mut c_void) != 0 {
            return;
        }
        // AVCC (4-byte lengths) → Annex-B (start codes)
        let mut i = 0;
        while i + 4 <= avcc.len() {
            let n = u32::from_be_bytes(avcc[i..i + 4].try_into().unwrap()) as usize;
            let end = (i + 4 + n).min(avcc.len());
            data.extend_from_slice(&[0, 0, 0, 1]);
            data.extend_from_slice(&avcc[i + 4..end]);
            i = end;
        }
        if src as usize == REFINE_TAG && std::env::var_os("MANYDESK_PERF").is_some() {
            eprintln!("[refine] {} bytes key={key}", data.len());
        }
        let p = out.video.params();
        let ok = out.sink.push(EncodedFrame { data: Bytes::from(data), key, duration: Duration::from_secs_f64(1.0 / p.fps) });
        let (w, h) = *out.size.lock().unwrap();
        let hw = out.hardware.load(std::sync::atomic::Ordering::Relaxed);
        out.video.record(ok, w, h, false, if hw { "VideoToolbox" } else { "VideoToolbox (software)" }, hw);
    }
}

// ---- ScreenCaptureKit output delegate ----

struct Ivars {
    out: Arc<Out>,
    enc: Mutex<Encoder>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ManyDeskScreenOutput"]
    #[ivars = Ivars]
    struct ScreenOutput;

    unsafe impl NSObjectProtocol for ScreenOutput {}

    unsafe impl SCStreamOutput for ScreenOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Screen {
                self.frame(sample as *const CMSampleBuffer as *const c_void);
            }
        }
    }
);

impl ScreenOutput {
    fn new(out: Arc<Out>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { out, enc: Mutex::new(Encoder::new()) });
        unsafe { msg_send![super(this), init] }
    }

    fn frame(&self, sb: *const c_void) {
        let iv = self.ivars();
        if iv.out.video.stopped() {
            return;
        }
        unsafe {
            // Only complete frames are new pictures (idle/blank ones repeat or carry nothing;
            // taking them for changes would cost a refinement each).
            let att = CMSampleBufferGetSampleAttachmentsArray(sb, 0);
            if !att.is_null() && CFArrayGetCount(att) > 0 {
                let n = CFDictionaryGetValue(CFArrayGetValueAtIndex(att, 0), SCStreamFrameInfoStatus as *const _ as CFTypeRef);
                let mut status: i32 = 0;
                if !n.is_null() && CFNumberGetValue(n, CF_SINT32, &mut status as *mut i32 as *mut c_void) != 0 && status != 0 {
                    crate::perf::tick("host-sck-notcomplete");
                    return;
                }
            }
            let img = CMSampleBufferGetImageBuffer(sb);
            if img.is_null() {
                return;
            }
            crate::perf::tick("sck-frame");
            let pts = CMSampleBufferGetPresentationTimeStamp(sb);
            let (w, h) = (CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img));
            let mut enc = iv.enc.lock().unwrap();
            // The path is backed up: skip, rather than queue a frame that only adds delay.
            // (Kept as the newest picture: if the screen stops here, the refinement sends it.)
            if iv.out.sink.congested() {
                iv.out.video.skipped();
                if enc.width == w && enc.height == h {
                    enc.keep(img, pts);
                }
                return;
            }
            if enc.session.is_null() || enc.width != w || enc.height != h {
                if let Err(st) = enc.open(Arc::as_ptr(&iv.out), w, h) {
                    eprintln!("[videotoolbox] session failed: {st}");
                    return;
                }
                *iv.out.size.lock().unwrap() = (w, h);
            }
            enc.apply(iv.out.video.params());
            let key = iv.out.video.take_keyframe();
            let t0 = Instant::now();
            let st = enc.encode(img, pts, key, false);
            enc.keep(img, pts);
            if st != 0 {
                eprintln!("[videotoolbox] encode failed: {st}");
            }
            let p = iv.out.video.params();
            if t0.elapsed() > Duration::from_secs_f64(1.0 / p.fps) {
                iv.out.video.record(true, w, h, true, "VideoToolbox", true);
            }
        }
    }
}

// ---- capture control ----

fn shareable_content() -> Result<Retained<SCShareableContent>, String> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let r = if content.is_null() {
            Err(if error.is_null() { "no shareable content".to_string() } else { unsafe { (*error).localizedDescription().to_string() } })
        } else {
            Ok(unsafe { Retained::retain(content).unwrap() })
        };
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    // Normally milliseconds; a little slack for a capture another app has in progress.
    rx.recv_timeout(Duration::from_secs(6)).map_err(|_| "screen recording permission?".to_string())?
}

/// A still picture of display `id`, `width` pixels wide (aspect kept), through
/// ScreenCaptureKit's own screenshot call. (CGWindowListCreateImage, which xcap uses, is
/// proxied through ScreenCaptureKit on current macOS and leaves it stuck for the rest of
/// the process: every stream started later timed out and fell back to software capture.)
pub fn screenshot(id: u32, width: usize) -> Option<image::RgbaImage> {
    let content = shareable_content().ok()?;
    let displays = unsafe { content.displays() };
    let display = displays.iter().find(|d| unsafe { d.displayID() } == id)?;
    let (fw, fh) = pixel_size(&display);
    let w = width.min(fw).max(16) & !1;
    let h = ((fh * w) / fw.max(1)).max(16) & !1;
    let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new()) };
    let cfg = unsafe { SCStreamConfiguration::new() };
    unsafe {
        cfg.setWidth(w);
        cfg.setHeight(h);
        cfg.setPixelFormat(BGRA);
        cfg.setShowsCursor(false);
    }
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |sb: *mut CMSampleBuffer, _error: *mut NSError| {
        let _ = tx.send(unsafe { bgra_to_rgba(sb as *const c_void) });
    });
    unsafe { SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(&filter, &cfg, Some(&block)) };
    rx.recv_timeout(Duration::from_secs(10)).ok().flatten()
}

unsafe fn bgra_to_rgba(sb: *const c_void) -> Option<image::RgbaImage> {
    if sb.is_null() {
        return None;
    }
    let img = CMSampleBufferGetImageBuffer(sb);
    if img.is_null() {
        return None;
    }
    CVPixelBufferLockBaseAddress(img, 1);
    let (w, h, stride, base) = (CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img), CVPixelBufferGetBytesPerRow(img), CVPixelBufferGetBaseAddress(img));
    let mut out = vec![0u8; w * h * 4];
    if !base.is_null() {
        for y in 0..h {
            let src = std::slice::from_raw_parts(base.add(y * stride), w * 4);
            for (d, s) in out[y * w * 4..(y + 1) * w * 4].chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
    }
    CVPixelBufferUnlockBaseAddress(img, 1);
    image::RgbaImage::from_raw(w as u32, h as u32, out)
}

/// A display's size in pixels (Retina: twice its size in points).
pub fn pixel_size_of(id: u32) -> Option<(usize, usize)> {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        if mode.is_null() {
            return None;
        }
        let size = (CGDisplayModeGetPixelWidth(mode), CGDisplayModeGetPixelHeight(mode));
        CGDisplayModeRelease(mode);
        Some(size)
    }
}

fn pixel_size(display: &SCDisplay) -> (usize, usize) {
    let id = unsafe { display.displayID() };
    pixel_size_of(id).unwrap_or_else(|| unsafe { (display.width() as usize, display.height() as usize) })
}

fn configure(cfg: &SCStreamConfiguration, full: (usize, usize), p: Params, cursor: bool) {
    let w = ((full.0 as f64 / p.scale) as usize).max(16) & !1;
    let h = ((full.1 as f64 / p.scale) as usize).max(16) & !1;
    unsafe {
        cfg.setWidth(w);
        cfg.setHeight(h);
        cfg.setPixelFormat(NV12);
        cfg.setMinimumFrameInterval(cm_time(1, p.fps.round().max(1.0) as i32));
        cfg.setShowsCursor(cursor);
        cfg.setQueueDepth(4);
    }
}

fn sync_call(f: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<(), String> {
    sync_call_for(Duration::from_secs(5), f)
}

fn sync_call_for(wait: Duration, f: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let _ = tx.send(if error.is_null() { Ok(()) } else { Err(unsafe { (*error).localizedDescription().to_string() }) });
    });
    f(&block);
    rx.recv_timeout(wait).map_err(|_| "timeout".to_string())?
}

/// Runs until the video is stopped or the sink closes. Err before the first frame means
/// "use the portable path instead" (e.g. no screen-recording permission).
pub fn run(video: Arc<Video>, sink: Arc<dyn Sink>) -> Result<(), String> {
    let t0 = Instant::now();
    let content = shareable_content();
    if std::env::var_os("MANYDESK_PERF").is_some() {
        eprintln!("[perf] SCShareableContent: {:?} after {:?}", content.as_ref().map(|_| "ok"), t0.elapsed());
    }
    let content = content?;
    let displays = unsafe { content.displays() };
    let want = video.screen().map(|s| s.id);
    let display = displays.iter().find(|d| Some(unsafe { d.displayID() }) == want).or_else(|| displays.iter().next()).ok_or("no display")?;
    let full = pixel_size(&display);

    let out = Arc::new(Out { video: video.clone(), sink: sink.clone(), size: Mutex::new((0, 0)), hardware: Default::default(), refine_dropped: Default::default() });
    let output = ScreenOutput::new(out);
    let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new()) };
    let cfg = unsafe { SCStreamConfiguration::new() };
    let mut applied = video.params();
    let mut cursor = video.cursor_in_picture();
    configure(&cfg, full, applied, cursor);
    let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &cfg, None) };
    let queue = DispatchQueue::new("manydesk.capture", DispatchQueueAttr::SERIAL);
    unsafe { stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue)) }
        .map_err(|e| e.localizedDescription().to_string())?;
    let t1 = Instant::now();
    let started = sync_call_for(Duration::from_secs(6), |b| unsafe { stream.startCaptureWithCompletionHandler(Some(b)) });
    if std::env::var_os("MANYDESK_PERF").is_some() {
        eprintln!("[perf] SCStream start: {started:?} after {:?}", t1.elapsed());
    }
    if let Err(e) = started {
        // It may still come up later: stop it, or it would hold the capture while the
        // software path (or the next hardware attempt) runs.
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
        return Err(e);
    }

    while !video.stopped() && !sink.closed() {
        std::thread::sleep(Duration::from_millis(50));
        let iv = output.ivars();
        iv.enc.lock().unwrap().idle(&iv.out);
        let p = video.params();
        // Size and frame rate are ScreenCaptureKit's job (GPU scaling); bitrate is applied per frame.
        if p.scale != applied.scale || p.fps != applied.fps || video.cursor_in_picture() != cursor {
            cursor = video.cursor_in_picture();
            configure(&cfg, full, p, cursor);
            let _ = sync_call(|b| unsafe { stream.updateConfiguration_completionHandler(&cfg, Some(b)) });
            applied = p;
        }
    }
    let _ = sync_call(|b| unsafe { stream.stopCaptureWithCompletionHandler(Some(b)) });
    // The encoder session closes when `output` (and its ivars) is released.
    drop(stream);
    Ok(())
}

// ---- the pointer, sent beside the video (position and shape over the control stream) ----

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl RefEncode for CGRect {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]));
}

/// Opaque CGImage, for a correctly typed `CGImageForProposedRect` return.
#[repr(C)]
struct CGImage {
    _p: [u8; 0],
}
unsafe impl RefEncode for CGImage {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(src: *const c_void) -> *const c_void;
    fn CGEventGetLocation(ev: *const c_void) -> CGPoint;
    fn CGDisplayBounds(id: u32) -> CGRect;
    fn CGImageGetWidth(img: *const CGImage) -> usize;
    fn CGImageGetHeight(img: *const CGImage) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
    fn CGColorSpaceRelease(cs: *const c_void);
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bits: usize, stride: usize, cs: *const c_void, info: u32) -> *mut c_void;
    fn CGContextDrawImage(ctx: *mut c_void, r: CGRect, img: *const CGImage);
    fn CGContextRelease(ctx: *mut c_void);
}

use super::CursorShape;

/// Where the pointer is on display `id` (0..1 each way; None when it is on another
/// display), and — with `shape` — what it looks like right now.
pub fn cursor(id: u32, shape: bool) -> (Option<(f64, f64)>, Option<CursorShape>) {
    unsafe {
        let b = CGDisplayBounds(id);
        let ev = CGEventCreate(ptr::null());
        if ev.is_null() || b.size.w <= 0.0 || b.size.h <= 0.0 {
            return (None, None);
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        let (x, y) = ((p.x - b.origin.x) / b.size.w, (p.y - b.origin.y) / b.size.h);
        let pos = ((0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)).then_some((x, y));
        // display pixels per point (2 on Retina)
        let scale = pixel_size_of(id).map(|(w, _)| w as f64 / b.size.w).unwrap_or(1.0);
        let shape = if shape { objc2::rc::autoreleasepool(|_| cursor_shape(scale)) } else { None };
        (pos, shape)
    }
}

unsafe fn cursor_shape(scale: f64) -> Option<CursorShape> {
    use objc2::runtime::{AnyClass, AnyObject};
    let cls = AnyClass::get(c"NSCursor")?;
    let cur: *mut AnyObject = msg_send![cls, currentSystemCursor];
    if cur.is_null() {
        return None;
    }
    let img: *mut AnyObject = msg_send![cur, image];
    if img.is_null() {
        return None;
    }
    let hot: CGPoint = msg_send![cur, hotSpot];
    let size: CGSize = msg_send![img, size];
    let cg: *const CGImage = msg_send![img, CGImageForProposedRect: ptr::null_mut::<CGRect>(), context: ptr::null_mut::<AnyObject>(), hints: ptr::null_mut::<AnyObject>()];
    if cg.is_null() || size.w <= 0.0 || size.h <= 0.0 {
        return None;
    }
    let (px, py) = (CGImageGetWidth(cg), CGImageGetHeight(cg));
    if px == 0 || py == 0 || px > 512 || py > 512 {
        return None;
    }
    let mut rgba = vec![0u8; px * py * 4];
    let cs = CGColorSpaceCreateDeviceRGB();
    // kCGImageAlphaPremultipliedLast | kCGBitmapByteOrderDefault: R G B A in memory
    let ctx = CGBitmapContextCreate(rgba.as_mut_ptr() as *mut c_void, px, py, 8, px * 4, cs, 1);
    CGColorSpaceRelease(cs);
    if ctx.is_null() {
        return None;
    }
    CGContextDrawImage(ctx, CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { w: px as f64, h: py as f64 } }, cg);
    CGContextRelease(ctx);
    Some(CursorShape { rgba, px, py, w: size.w * scale, h: size.h * scale, hx: hot.x * scale, hy: hot.y * scale })
}
