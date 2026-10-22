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
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType,
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
    fn CFNumberCreate(a: CFTypeRef, t: isize, v: *const c_void) -> CFTypeRef;
    fn CFArrayCreate(a: CFTypeRef, v: *const CFTypeRef, n: isize, cb: *const c_void) -> CFTypeRef;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
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
}

struct Encoder {
    session: *mut c_void,
    width: usize,
    height: usize,
    params: Option<Params>,
}

unsafe impl Send for Encoder {}

impl Encoder {
    fn close(&mut self) {
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
            set(s, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, cf_f64(10.0), true);
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

extern "C" fn on_encoded(refcon: *mut c_void, _src: *mut c_void, status: i32, _flags: u32, sb: *const c_void) {
    if status != 0 || sb.is_null() || refcon.is_null() {
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
        let this = Self::alloc().set_ivars(Ivars { out, enc: Mutex::new(Encoder { session: ptr::null_mut(), width: 0, height: 0, params: None }) });
        unsafe { msg_send![super(this), init] }
    }

    fn frame(&self, sb: *const c_void) {
        let iv = self.ivars();
        if iv.out.video.stopped() {
            return;
        }
        unsafe {
            // Idle/blank frames (nothing changed) carry no image.
            let img = CMSampleBufferGetImageBuffer(sb);
            if img.is_null() {
                return;
            }
            let (w, h) = (CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img));
            let mut enc = iv.enc.lock().unwrap();
            if enc.session.is_null() || enc.width != w || enc.height != h {
                if let Err(st) = enc.open(Arc::as_ptr(&iv.out), w, h) {
                    eprintln!("[videotoolbox] session failed: {st}");
                    return;
                }
                *iv.out.size.lock().unwrap() = (w, h);
            }
            enc.apply(iv.out.video.params());
            let props = if iv.out.video.take_keyframe() {
                let d = CFDictionaryCreateMutable(ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
                CFDictionarySetValue(d, kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue);
                d as CFTypeRef
            } else {
                ptr::null()
            };
            let pts = CMSampleBufferGetPresentationTimeStamp(sb);
            let t0 = Instant::now();
            let st = VTCompressionSessionEncodeFrame(enc.session, img, pts, cm_time(0, 0), props, ptr::null_mut(), ptr::null_mut());
            if !props.is_null() {
                CFRelease(props);
            }
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
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "screen recording permission?".to_string())?
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

fn configure(cfg: &SCStreamConfiguration, full: (usize, usize), p: Params) {
    let w = ((full.0 as f64 / p.scale) as usize).max(16) & !1;
    let h = ((full.1 as f64 / p.scale) as usize).max(16) & !1;
    unsafe {
        cfg.setWidth(w);
        cfg.setHeight(h);
        cfg.setPixelFormat(NV12);
        cfg.setMinimumFrameInterval(cm_time(1, p.fps.round().max(1.0) as i32));
        cfg.setShowsCursor(true);
        cfg.setQueueDepth(4);
    }
}

fn sync_call(f: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let _ = tx.send(if error.is_null() { Ok(()) } else { Err(unsafe { (*error).localizedDescription().to_string() }) });
    });
    f(&block);
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "timeout".to_string())?
}

/// Runs until the video is stopped or the sink closes. Err before the first frame means
/// "use the portable path instead" (e.g. no screen-recording permission).
pub fn run(video: Arc<Video>, sink: Arc<dyn Sink>) -> Result<(), String> {
    let content = shareable_content()?;
    let displays = unsafe { content.displays() };
    let want = video.screen().map(|s| s.id);
    let display = displays.iter().find(|d| Some(unsafe { d.displayID() }) == want).or_else(|| displays.iter().next()).ok_or("no display")?;
    let full = pixel_size(&display);

    let out = Arc::new(Out { video: video.clone(), sink: sink.clone(), size: Mutex::new((0, 0)), hardware: Default::default() });
    let output = ScreenOutput::new(out);
    let filter = unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new()) };
    let cfg = unsafe { SCStreamConfiguration::new() };
    let mut applied = video.params();
    configure(&cfg, full, applied);
    let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &cfg, None) };
    let queue = DispatchQueue::new("manydesk.capture", DispatchQueueAttr::SERIAL);
    unsafe { stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue)) }
        .map_err(|e| e.localizedDescription().to_string())?;
    sync_call(|b| unsafe { stream.startCaptureWithCompletionHandler(Some(b)) })?;

    while !video.stopped() && !sink.closed() {
        std::thread::sleep(Duration::from_millis(100));
        let p = video.params();
        // Size and frame rate are ScreenCaptureKit's job (GPU scaling); bitrate is applied per frame.
        if p.scale != applied.scale || p.fps != applied.fps {
            configure(&cfg, full, p);
            let _ = sync_call(|b| unsafe { stream.updateConfiguration_completionHandler(&cfg, Some(b)) });
            applied = p;
        }
    }
    let _ = sync_call(|b| unsafe { stream.stopCaptureWithCompletionHandler(Some(b)) });
    // The encoder session closes when `output` (and its ivars) is released.
    drop(stream);
    Ok(())
}
