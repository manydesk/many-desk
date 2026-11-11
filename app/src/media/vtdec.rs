// macOS viewer video: H.264 → VideoToolbox (hardware) → NV12 (video range), which the
// GPU turns into RGB while drawing. Annex-B access units come in; SPS/PPS (sent with every
// keyframe) (re)create the decoder session, the other NAL units are repacked with 4-byte
// lengths (AVCC).
use std::ffi::c_void;
use std::ptr;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct OutputCallback {
    cb: extern "C" fn(*mut c_void, *mut c_void, i32, u32, *mut c_void, CMTime, CMTime),
    refcon: *mut c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreateMutable(a: CFTypeRef, cap: isize, k: *const c_void, v: *const c_void) -> *mut c_void;
    fn CFDictionarySetValue(d: *mut c_void, k: CFTypeRef, v: CFTypeRef);
    fn CFNumberCreate(a: CFTypeRef, t: isize, v: *const c_void) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        a: CFTypeRef,
        count: usize,
        ptrs: *const *const u8,
        sizes: *const usize,
        nal_len: i32,
        out: *mut *const c_void,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        a: CFTypeRef,
        mem: *mut c_void,
        len: usize,
        block_alloc: CFTypeRef,
        custom: *const c_void,
        off: usize,
        data_len: usize,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn CMBlockBufferReplaceDataBytes(src: *const c_void, bb: *mut c_void, off: usize, len: usize) -> i32;
    fn CMSampleBufferCreateReady(
        a: CFTypeRef,
        data: *mut c_void,
        fmt: *const c_void,
        n: isize,
        n_timing: isize,
        timing: *const c_void,
        n_sizes: isize,
        sizes: *const usize,
        out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(pb: *mut c_void, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(pb: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(pb: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferGetWidth(pb: *const c_void) -> usize;
    fn CVPixelBufferGetHeight(pb: *const c_void) -> usize;
    fn CVPixelBufferGetPixelFormatType(pb: *const c_void) -> u32;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTDecompressionSessionCreate(
        a: CFTypeRef,
        fmt: *const c_void,
        spec: CFTypeRef,
        attrs: CFTypeRef,
        cb: *const OutputCallback,
        out: *mut *mut c_void,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(s: *mut c_void, sb: *mut c_void, flags: u32, rc: *mut c_void, info: *mut u32) -> i32;
    fn VTDecompressionSessionWaitForAsynchronousFrames(s: *mut c_void) -> i32;
    fn VTDecompressionSessionInvalidate(s: *mut c_void);
}

/// NV12, video range (16–235): the decoder's own layout, half the bytes of BGRA
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const CF_SINT32: isize = 3;
const ASSURE_MEMORY_NOW: u32 = 1;

/// Where the output callback leaves the picture.
#[derive(Default)]
struct Out {
    /// the buffer to fill (a spare one from the viewer), and the picture made of it
    buf: Vec<u8>,
    pic: Option<crate::video::Picture>,
    color: crate::video::Color,
    error: bool,
}

extern "C" fn on_frame(refcon: *mut c_void, _src: *mut c_void, status: i32, _flags: u32, img: *mut c_void, _pts: CMTime, _dur: CMTime) {
    let out = unsafe { &mut *(refcon as *mut Out) };
    if status != 0 || img.is_null() {
        out.error = true;
        return;
    }
    unsafe {
        CVPixelBufferLockBaseAddress(img, 1);
        let (w, h) = (CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img));
        let fmt = CVPixelBufferGetPixelFormatType(img);
        let (y, uv) = (CVPixelBufferGetBaseAddressOfPlane(img, 0), CVPixelBufferGetBaseAddressOfPlane(img, 1));
        if !y.is_null() && !uv.is_null() && (fmt == NV12_VIDEO || fmt == NV12_FULL) {
            // the two planes, rows as VideoToolbox lays them out (one copy, no conversion)
            let (ys, uvs) = (CVPixelBufferGetBytesPerRowOfPlane(img, 0), CVPixelBufferGetBytesPerRowOfPlane(img, 1));
            let (yh, uvh) = (CVPixelBufferGetHeightOfPlane(img, 0), CVPixelBufferGetHeightOfPlane(img, 1));
            let mut data = std::mem::take(&mut out.buf);
            data.clear();
            data.extend_from_slice(std::slice::from_raw_parts(y, ys * yh));
            let uv_offset = data.len();
            data.extend_from_slice(std::slice::from_raw_parts(uv, uvs * uvh));
            let color = crate::video::Color { bt709: out.color.bt709, full: fmt == NV12_FULL };
            out.pic = Some(crate::video::Picture { width: w, height: h, planes: crate::video::Planes::Nv12 { y_stride: ys, uv_offset, uv_stride: uvs }, data, color });
        } else {
            out.error = true;
        }
        CVPixelBufferUnlockBaseAddress(img, 1);
    }
}

pub struct VtDecoder {
    session: *mut c_void,
    fmt: *const c_void,
    sps: Vec<u8>,
    pps: Vec<u8>,
    out: Box<Out>,
}

unsafe impl Send for VtDecoder {}

impl VtDecoder {
    pub fn new() -> Result<Self, String> {
        Ok(VtDecoder { session: ptr::null_mut(), fmt: ptr::null(), sps: vec![], pps: vec![], out: Box::default() })
    }

    pub fn name(&self) -> &'static str {
        "VideoToolbox"
    }

    pub fn reset(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = ptr::null_mut();
            }
            if !self.fmt.is_null() {
                CFRelease(self.fmt);
                self.fmt = ptr::null();
            }
        }
    }

    fn open(&mut self) -> Result<(), String> {
        self.reset();
        unsafe {
            let sets = [self.sps.as_ptr(), self.pps.as_ptr()];
            let sizes = [self.sps.len(), self.pps.len()];
            let mut fmt = ptr::null();
            let st = CMVideoFormatDescriptionCreateFromH264ParameterSets(ptr::null(), 2, sets.as_ptr(), sizes.as_ptr(), 4, &mut fmt);
            if st != 0 {
                return Err(format!("format {st}"));
            }
            self.fmt = fmt;
            let attrs = CFDictionaryCreateMutable(ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            let pf = NV12_VIDEO as i32;
            let n = CFNumberCreate(ptr::null(), CF_SINT32, &pf as *const i32 as *const c_void);
            CFDictionarySetValue(attrs, kCVPixelBufferPixelFormatTypeKey, n);
            CFRelease(n);
            let cb = OutputCallback { cb: on_frame, refcon: &mut *self.out as *mut Out as *mut c_void };
            let mut s = ptr::null_mut();
            let st = VTDecompressionSessionCreate(ptr::null(), fmt, ptr::null(), attrs, &cb, &mut s);
            CFRelease(attrs);
            if st != 0 {
                return Err(format!("session {st}"));
            }
            self.session = s;
        }
        Ok(())
    }

    /// Ok(Some(..)): a new picture (NV12 in `buf`). Err: decoder error.
    pub fn decode(&mut self, au: &[u8], buf: Vec<u8>, color: crate::video::Color) -> Result<Option<crate::video::Picture>, String> {
        let mut avcc = Vec::with_capacity(au.len() + 16);
        let mut new_params = false;
        for n in super::decode::nals(au) {
            match n[0] & 0x1f {
                7 => {
                    if n != self.sps.as_slice() {
                        self.sps = n.to_vec();
                        new_params = true;
                    }
                }
                8 => {
                    if n != self.pps.as_slice() {
                        self.pps = n.to_vec();
                        new_params = true;
                    }
                }
                9 => {}
                _ => {
                    avcc.extend_from_slice(&(n.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(n);
                }
            }
        }
        if (new_params || self.session.is_null()) && !self.sps.is_empty() && !self.pps.is_empty() {
            self.open()?;
        }
        if self.session.is_null() {
            return Err("no parameter sets yet".into());
        }
        if avcc.is_empty() {
            return Ok(None);
        }
        self.out.pic = None;
        self.out.buf = buf;
        self.out.color = color;
        self.out.error = false;
        unsafe {
            let mut bb = ptr::null_mut();
            if CMBlockBufferCreateWithMemoryBlock(ptr::null(), ptr::null_mut(), avcc.len(), ptr::null(), ptr::null(), 0, avcc.len(), ASSURE_MEMORY_NOW, &mut bb) != 0 {
                return Err("block buffer".into());
            }
            CMBlockBufferReplaceDataBytes(avcc.as_ptr() as *const c_void, bb, 0, avcc.len());
            let mut sb = ptr::null_mut();
            let size = avcc.len();
            let st = CMSampleBufferCreateReady(ptr::null(), bb, self.fmt, 1, 0, ptr::null(), 1, &size, &mut sb);
            CFRelease(bb);
            if st != 0 {
                return Err(format!("sample {st}"));
            }
            // flags 0: decode synchronously; the callback runs before this returns.
            let mut info = 0u32;
            let st = VTDecompressionSessionDecodeFrame(self.session, sb, 0, ptr::null_mut(), &mut info);
            CFRelease(sb);
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            if st != 0 {
                // -12909 (bad data) / -12911 (malfunction): start over from the next keyframe.
                self.reset();
                return Err(format!("decode {st}"));
            }
        }
        if self.out.error {
            return Err("decode callback".into());
        }
        Ok(self.out.pic.take())
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        self.reset();
    }
}
