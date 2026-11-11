// Windows host video on the GPU, as the macOS path does with ScreenCaptureKit:
//
//   DXGI Desktop Duplication → copy of the desktop texture → D3D11 video processor
//   (BGRA → NV12 and scaling, on the GPU) → hardware H.264 (Media Foundation, the texture
//   itself goes in) — or, without one, the NV12 read back (1.5 bytes a pixel, no colour
//   conversion on the CPU) into OpenH264 on several threads.
//
// Duplication says when the desktop changed (no comparing frames on the CPU); a still
// screen costs nothing, and once it has been still for a moment the last picture is coded
// again a couple of times, so text ends up crisp. The pointer is not in the picture: the
// viewer draws it from Session::cursor_apart (`cursor` below).
//
// Any failure falls back to the portable path in encode.rs. Desktop switches (UAC, the
// lock screen, a resolution change) end the duplication; it is opened again.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use openh264::encoder::{BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::{MFCreateDXGIDeviceManager, IMFDXGIDeviceManager};

use super::encode::{EncodedFrame, Sink, Video, KEY_EVERY_SECS};
use super::win::MfEncoder;
use super::CursorShape;

/// The GPU path can't work on this machine (no duplication or no video processor): the
/// portable path for good, instead of trying again every few seconds.
static UNSUPPORTED: AtomicBool = AtomicBool::new(false);

pub fn unsupported() -> bool {
    UNSUPPORTED.load(Ordering::Relaxed)
}

/// The screen counts as still after this long, and is then coded again this many times.
const STILL: Duration = Duration::from_millis(300);
const REFINE_PASSES: u8 = 2;
/// OpenH264 slice size limit (see `software`).
const SLICE_BYTES: u32 = super::SLICE_BYTES;
/// NV12 textures in turn: the hardware encoder may still be reading the last ones.
const RING: usize = 4;

struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    output: IDXGIOutput1,
    manager: IMFDXGIDeviceManager,
}

/// The device of the GPU that drives display `id` (an HMONITOR), and that output.
fn open_gpu(id: Option<u32>) -> Result<Gpu, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("DXGI factory: {e}"))?;
        let mut a = 0;
        let mut first: Option<(IDXGIAdapter1, IDXGIOutput1)> = None;
        let mut found: Option<(IDXGIAdapter1, IDXGIOutput1)> = None;
        'adapters: while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(desc) = output.GetDesc() else { continue };
                let Ok(o1) = output.cast::<IDXGIOutput1>() else { continue };
                if first.is_none() {
                    first = Some((adapter.clone(), o1.clone()));
                }
                if id.is_some_and(|id| desc.Monitor.0 as usize as u32 == id) {
                    found = Some((adapter.clone(), o1));
                    break 'adapters;
                }
            }
        }
        let (adapter, output) = found.or(first).ok_or("no display output")?;
        let mut device = None;
        let mut ctx = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| format!("D3D11 device: {e}"))?;
        let device: ID3D11Device = device.ok_or("no D3D11 device")?;
        let ctx = ctx.ok_or("no D3D11 context")?;
        // the encoder works on the device from its own threads
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let vdev: ID3D11VideoDevice = device.cast().map_err(|e| format!("no video device: {e}"))?;
        let vctx: ID3D11VideoContext = ctx.cast().map_err(|e| format!("no video context: {e}"))?;
        let mut token = 0u32;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| format!("DXGI device manager: {e}"))?;
        let manager = manager.ok_or("no DXGI device manager")?;
        manager.ResetDevice(&device, token).map_err(|e| format!("DXGI device manager: {e}"))?;
        Ok(Gpu { device, ctx, vdev, vctx, output, manager })
    }
}

fn texture(dev: &ID3D11Device, w: u32, h: u32, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, usage: D3D11_USAGE, bind: u32, cpu: u32) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { dev.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

/// Desktop → NV12 at the coded size, on the GPU.
struct Converter {
    src: (u32, u32),
    dst: (u32, u32),
    /// our copy of the desktop (the duplication's own texture is only ours until ReleaseFrame)
    copy: ID3D11Texture2D,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    out: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// read back for the software encoder
    staging: Option<ID3D11Texture2D>,
}

impl Converter {
    fn new(g: &Gpu, src: (u32, u32), dst: (u32, u32), for_encoder: bool) -> windows::core::Result<Self> {
        unsafe {
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: src.0,
                InputHeight: src.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: dst.0,
                OutputHeight: dst.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
            };
            let en = g.vdev.CreateVideoProcessorEnumerator(&content)?;
            let fmt = en.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?;
            if fmt & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_NOTIMPL));
            }
            let processor = g.vdev.CreateVideoProcessor(&en, 0)?;
            let copy = texture(&g.device, src.0, src.1, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0)?;
            let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut input = None;
            g.vdev.CreateVideoProcessorInputView(&copy, &en, &ivd, Some(&mut input))?;
            let input = input.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
            // the encoder reads them as video input
            let bind = D3D11_BIND_RENDER_TARGET.0 as u32 | if for_encoder { D3D11_BIND_VIDEO_ENCODER.0 as u32 } else { 0 };
            let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out = Vec::new();
            for _ in 0..if for_encoder { RING } else { 1 } {
                let t = match texture(&g.device, dst.0, dst.1, DXGI_FORMAT_NV12, D3D11_USAGE_DEFAULT, bind, 0) {
                    Ok(t) => t,
                    // some drivers refuse the encoder bind flag: plain render targets then
                    Err(_) => texture(&g.device, dst.0, dst.1, DXGI_FORMAT_NV12, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0)?,
                };
                let mut v = None;
                g.vdev.CreateVideoProcessorOutputView(&t, &en, &ovd, Some(&mut v))?;
                out.push((t, v.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?));
            }
            let staging = if for_encoder { None } else { Some(texture(&g.device, dst.0, dst.1, DXGI_FORMAT_NV12, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32)?) };
            // RGB 0–255 in; YCbCr BT.601 16–235 out (what the decoders on the other side assume)
            g.vctx.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            g.vctx.VideoProcessorSetOutputColorSpace(&processor, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 4 });
            g.vctx.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // no driver "enhancements" on text
            g.vctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let s = RECT { left: 0, top: 0, right: src.0 as i32, bottom: src.1 as i32 };
            let d = RECT { left: 0, top: 0, right: dst.0 as i32, bottom: dst.1 as i32 };
            g.vctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&s));
            g.vctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&d));
            g.vctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&d));
            Ok(Converter { src, dst, copy, processor, input, out, next: 0, staging })
        }
    }

    /// Converts the current copy into the next NV12 texture of the ring; returns its index.
    fn convert(&mut self, g: &Gpu) -> windows::core::Result<usize> {
        let i = self.next;
        self.next = (self.next + 1) % self.out.len();
        let stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())), ..Default::default() };
        let streams = [stream];
        let r = unsafe { g.vctx.VideoProcessorBlt(&self.processor, &self.out[i].1, 0, &streams) };
        let [mut stream] = streams;
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        r.map(|_| i)
    }

    /// NV12 texture `i` → I420 planes (for OpenH264).
    fn read_i420(&self, g: &Gpu, i: usize, out: &mut Vec<u8>) -> windows::core::Result<()> {
        let staging = self.staging.as_ref().expect("software path");
        let (w, h) = (self.dst.0 as usize, self.dst.1 as usize);
        let (cw, ch) = (w / 2, h / 2);
        out.resize(w * h + 2 * cw * ch, 0);
        unsafe {
            g.ctx.CopyResource(staging, &self.out[i].0);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            g.ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let pitch = m.RowPitch as usize;
            let base = m.pData as *const u8;
            for y in 0..h {
                let row = std::slice::from_raw_parts(base.add(y * pitch), w);
                out[y * w..(y + 1) * w].copy_from_slice(row);
            }
            // the interleaved UV plane follows the Y plane (its rows padded to the pitch)
            let uv = base.add(pitch * h);
            let (u, v) = out[w * h..].split_at_mut(cw * ch);
            for y in 0..ch {
                let row = std::slice::from_raw_parts(uv.add(y * pitch), cw * 2);
                for x in 0..cw {
                    u[y * cw + x] = row[2 * x];
                    v[y * cw + x] = row[2 * x + 1];
                }
            }
            g.ctx.Unmap(staging, 0);
        }
        Ok(())
    }
}

enum Enc {
    Hardware(MfEncoder),
    Software(Encoder, Vec<u8>),
}

fn software(fps: f64, bitrate: u32) -> Result<Encoder, String> {
    // every core helps here (a GPU without an encoder, a VM): up to 4 encoder threads
    let threads = threads() as u16;
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .complexity(Complexity::Low)
        .num_threads(threads)
        // slices of at most this many bytes: a big frame (a scroll) splits and the threads
        // share it (one slice per frame keeps OpenH264 on a single thread)
        .max_slice_len(SLICE_BYTES)
        .skip_frames(true)
        .intra_frame_period(IntraFramePeriod::from_num_frames((fps * KEY_EVERY_SECS) as u32));
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| e.to_string())
}

/// The coded size for scale `scale` of a `w`×`h` desktop (even, as NV12 needs).
fn coded(w: u32, h: u32, scale: f64) -> (u32, u32) {
    (((w as f64 / scale) as u32).max(16) & !1, ((h as f64 / scale) as u32).max(16) & !1)
}

/// Runs until the video stops or the sink closes (Ok), or the GPU path can't go on (Err:
/// the caller takes the portable path).
pub fn run(video: Arc<Video>, sink: Arc<dyn Sink>) -> Result<(), String> {
    let id = video.screen().map(|s| s.id);
    let g = match open_gpu(id) {
        Ok(g) => g,
        Err(e) => {
            UNSUPPORTED.store(true, Ordering::Relaxed);
            return Err(e);
        }
    };
    let mut dup = duplicate(&g)?;
    let mut src = desktop_size(&dup);
    // starts with the hardware encoder in mind; without one it reads frames back instead
    let mut hardware_ok = true;
    let p0 = video.params();
    let mut conv = new_converter(&g, src, coded(src.0, src.1, p0.scale), hardware_ok, None)?;
    let mut enc: Option<(Enc, super::encode::Params, (u32, u32))> = None;
    let mut have_picture = false;
    let mut last_change = Instant::now();
    let mut refined = REFINE_PASSES;
    let mut lost_at: Option<Instant> = None;
    eprintln!("[wincap] GPU capture of {}x{}", src.0, src.1);
    while !video.stopped() && !sink.closed() {
        let p = video.params();
        let interval = Duration::from_secs_f64(1.0 / p.fps);
        let t0 = Instant::now();
        if sink.congested() {
            video.skipped();
            std::thread::sleep(interval);
            continue;
        }
        // a new desktop picture?
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        let wait = interval.as_millis().clamp(1, 100) as u32;
        let changed = match unsafe { dup.AcquireNextFrame(wait, &mut info, &mut res) } {
            Ok(()) => {
                // a pointer move alone (LastPresentTime 0) is not a new picture
                let tex = res.as_ref().filter(|_| info.LastPresentTime != 0).and_then(|r| r.cast::<ID3D11Texture2D>().ok());
                let fresh = tex.is_some();
                if let Some(tex) = tex {
                    let mut d = D3D11_TEXTURE2D_DESC::default();
                    unsafe { tex.GetDesc(&mut d) };
                    if (d.Width, d.Height) != src {
                        src = (d.Width, d.Height);
                        conv = new_converter(&g, src, coded(src.0, src.1, p.scale), hardware_ok, None)?;
                        enc = None;
                    }
                    unsafe { g.ctx.CopyResource(&conv.copy, &tex) };
                }
                unsafe {
                    let _ = dup.ReleaseFrame();
                }
                lost_at = None;
                fresh
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => false,
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == DXGI_ERROR_INVALID_CALL => {
                // a desktop switch or a mode change: open it again (the secure desktop may
                // take a moment; past that, the portable path takes over for a while)
                let since = *lost_at.get_or_insert_with(Instant::now);
                drop(res);
                std::thread::sleep(Duration::from_millis(100));
                match duplicate(&g) {
                    Ok(d) => {
                        dup = d;
                        let s = desktop_size(&dup);
                        if s != src {
                            src = s;
                            conv = new_converter(&g, src, coded(src.0, src.1, p.scale), hardware_ok, None)?;
                            enc = None;
                        }
                        // the first frame of a new duplication is the whole desktop
                        have_picture = false;
                    }
                    Err(e) if since.elapsed() > Duration::from_secs(3) => return Err(format!("desktop duplication lost: {e}")),
                    Err(_) => {}
                }
                continue;
            }
            Err(e) => return Err(format!("AcquireNextFrame: {e}")),
        };
        if changed {
            have_picture = true;
            last_change = Instant::now();
            refined = 0;
        }
        let force = video.take_keyframe();
        let refine = !changed && have_picture && refined < REFINE_PASSES && last_change.elapsed() >= STILL;
        if !have_picture || (!changed && !force && !refine) {
            if force {
                video.request_keyframe(); // still owed once there is a picture
            }
            continue;
        }
        if refine {
            refined += 1;
        }
        // a new size: the converter follows (it keeps the current picture)
        let dst = coded(src.0, src.1, p.scale);
        if conv.dst != dst {
            conv = new_converter(&g, src, dst, hardware_ok, Some(&conv))?;
            enc = None;
        }
        // a new encoder for a new size or frame rate; the bitrate alone changes in place on
        // the hardware one
        let need = match &mut enc {
            Some((Enc::Hardware(m), ep, d)) if *d == dst && ep.fps == p.fps => {
                if ep.bitrate != p.bitrate {
                    m.set_bitrate(p.bitrate);
                    ep.bitrate = p.bitrate;
                }
                false
            }
            Some((Enc::Software(..), ep, d)) => *d != dst || ep.fps != p.fps || ep.bitrate != p.bitrate,
            _ => true,
        };
        let mut force = force;
        if need {
            force = true;
            enc = None;
            if hardware_ok {
                match MfEncoder::new(dst.0 as usize, dst.1 as usize, p.fps, p.bitrate, Some(&g.manager)) {
                    Ok(m) => {
                        eprintln!("[wincap] hardware encoder: {} ({}x{})", m.name, dst.0, dst.1);
                        enc = Some((Enc::Hardware(m), p, dst));
                    }
                    Err(e) => {
                        eprintln!("[wincap] no hardware encoder for textures ({e}); OpenH264 on {} threads", threads());
                        hardware_ok = false;
                        conv = new_converter(&g, src, dst, false, Some(&conv))?;
                    }
                }
            }
            if enc.is_none() {
                enc = Some((Enc::Software(software(p.fps, p.bitrate)?, Vec::new()), p, dst));
            }
        }
        let tc = Instant::now();
        let i = conv.convert(&g).map_err(|e| format!("video processor: {e}"))?;
        crate::perf::add("gpu-convert", tc.elapsed());
        let te = Instant::now();
        let result = match &mut enc.as_mut().unwrap().0 {
            Enc::Hardware(m) => m.encode_texture(&conv.out[i].0, force).map(|f| (f, "Media Foundation (GPU)", true)).map_err(|e| e.to_string()),
            Enc::Software(e, yuv) => {
                conv.read_i420(&g, i, yuv).map_err(|e| format!("read back: {e}"))?;
                crate::perf::add("gpu-readback", te.elapsed());
                if force {
                    e.force_intra_frame();
                }
                let (w, h) = (dst.0 as usize, dst.1 as usize);
                let cw = w / 2;
                let (y, uv) = yuv.split_at(w * h);
                let (u, v) = uv.split_at(cw * (h / 2));
                match e.encode(&YUVSlices::new((y, u, v), (w, h), (w, cw, cw))) {
                    Ok(bs) => Ok((vec![(bs.to_vec(), matches!(bs.frame_type(), FrameType::IDR | FrameType::I))], "OpenH264 (GPU capture)", false)),
                    Err(_) => continue,
                }
            }
        };
        let (frames, implementation, hardware) = match result {
            Ok(r) => r,
            Err(e) => {
                // the hardware encoder gave up: OpenH264 from here on
                eprintln!("[wincap] hardware encoder failed ({e}); OpenH264");
                hardware_ok = false;
                enc = None;
                conv = new_converter(&g, src, dst, false, Some(&conv))?;
                video.request_keyframe();
                continue;
            }
        };
        crate::perf::add("gpu-encode", te.elapsed());
        for (data, key) in frames {
            if data.is_empty() {
                continue;
            }
            let ok = sink.push(EncodedFrame { data: Bytes::from(data), key, duration: interval });
            video.record(ok, dst.0 as usize, dst.1 as usize, t0.elapsed() > interval.mul_f64(1.5), implementation, hardware);
            video.timing(t0.elapsed(), interval);
        }
        let spent = t0.elapsed();
        if spent < interval {
            std::thread::sleep(interval - spent);
        }
    }
    Ok(())
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2)
}

/// A converter for these sizes; with `keep`, it starts with that one's picture (a still
/// screen sends no new frame to fill it).
fn new_converter(g: &Gpu, src: (u32, u32), dst: (u32, u32), for_encoder: bool, keep: Option<&Converter>) -> Result<Converter, String> {
    let c = Converter::new(g, src, dst, for_encoder).map_err(|e| {
        UNSUPPORTED.store(true, Ordering::Relaxed);
        format!("video processor {}x{} → {}x{}: {e}", src.0, src.1, dst.0, dst.1)
    })?;
    if let Some(old) = keep.filter(|o| o.src == src) {
        unsafe { g.ctx.CopyResource(&c.copy, &old.copy) };
    }
    Ok(c)
}

fn duplicate(g: &Gpu) -> Result<IDXGIOutputDuplication, String> {
    unsafe { g.output.DuplicateOutput(&g.device) }.map_err(|e| {
        // no duplication at all (a remote session, an unsupported driver): not again
        if e.code() == DXGI_ERROR_UNSUPPORTED || e.code() == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
            UNSUPPORTED.store(true, Ordering::Relaxed);
        }
        format!("DuplicateOutput: {e}")
    })
}

fn desktop_size(d: &IDXGIOutputDuplication) -> (u32, u32) {
    let desc = unsafe { d.GetDesc() };
    (desc.ModeDesc.Width, desc.ModeDesc.Height)
}

// ---- the pointer, for a viewer that draws it itself ----

/// Where the pointer is on display `id` (an HMONITOR; 0..1 each way, None when it is on
/// another display or hidden), and — with `shape` — what it looks like right now.
pub fn cursor(id: u32, shape: bool) -> (Option<(f64, f64)>, Option<CursorShape>) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorInfo, CURSORINFO, CURSOR_SHOWING};
    unsafe {
        let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
        if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
            return (None, None);
        }
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(HMONITOR(id as usize as *mut _), &mut mi).as_bool() {
            return (None, None);
        }
        let r = mi.rcMonitor;
        let (w, h) = ((r.right - r.left) as f64, (r.bottom - r.top) as f64);
        if w <= 0.0 || h <= 0.0 {
            return (None, None);
        }
        let (x, y) = ((ci.ptScreenPos.x - r.left) as f64 / w, (ci.ptScreenPos.y - r.top) as f64 / h);
        let pos = ((0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)).then_some((x, y));
        let shape = if shape && !ci.hCursor.is_invalid() { cursor_shape(ci.hCursor) } else { None };
        (pos, shape)
    }
}

/// The cursor's pixels: colour cursors with their alpha (or their mask), and the classic
/// two-colour ones (the text I-beam among them) from their AND/XOR masks.
unsafe fn cursor_shape(cur: windows::Win32::UI::WindowsAndMessaging::HCURSOR) -> Option<CursorShape> {
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ};
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO};
    let mut ii = ICONINFO::default();
    GetIconInfo(HICON(cur.0), &mut ii).ok()?;
    let read = |bm: HBITMAP| -> Option<(Vec<u8>, usize, usize)> {
        if bm.is_invalid() {
            return None;
        }
        let mut b = BITMAP::default();
        if GetObjectW(HGDIOBJ(bm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut b as *mut _ as *mut _)) == 0 {
            return None;
        }
        let (w, h) = (b.bmWidth as usize, b.bmHeight as usize);
        if w == 0 || h == 0 || w > 256 || h > 512 {
            return None;
        }
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() };
        let mut px = vec![0u8; w * h * 4];
        let dc = GetDC(None);
        let n = GetDIBits(dc, bm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        (n > 0).then_some((px, w, h))
    };
    let color = read(ii.hbmColor);
    let mask = read(ii.hbmMask);
    if !ii.hbmColor.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
    }
    if !ii.hbmMask.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
    }
    let (hx, hy) = (ii.xHotspot as f64, ii.yHotspot as f64);
    let (rgba, w, h) = match (color, mask) {
        (Some((bgra, w, h)), mask) => {
            let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
            let mut out = vec![0u8; w * h * 4];
            for i in 0..w * h {
                let (b, g, r, a) = (bgra[i * 4], bgra[i * 4 + 1], bgra[i * 4 + 2], bgra[i * 4 + 3]);
                // no alpha channel: the AND mask says what is transparent
                let a = if has_alpha { a } else if mask.as_ref().is_some_and(|(m, ..)| m[i * 4] != 0) { 0 } else { 255 };
                let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
                // colour cursors from GetDIBits are already premultiplied when they carry alpha
                let (r, g, b) = if has_alpha { (r, g, b) } else { (pm(r), pm(g), pm(b)) };
                out[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
            }
            (out, w, h)
        }
        (None, Some((m, w, h2))) => {
            // AND mask on top, XOR mask below
            let h = h2 / 2;
            let mut out = vec![0u8; w * h * 4];
            for i in 0..w * h {
                let and = m[i * 4] != 0;
                let xor = m[(i + w * h) * 4] != 0;
                let px: [u8; 4] = match (and, xor) {
                    (false, false) => [0, 0, 0, 255],
                    (false, true) => [255, 255, 255, 255],
                    (true, false) => [0, 0, 0, 0],
                    // "invert the screen": black, so it shows on the usual light background
                    (true, true) => [0, 0, 0, 255],
                };
                out[i * 4..i * 4 + 4].copy_from_slice(&px);
            }
            // an inverted shape (the I-beam) gets a white edge, to show on dark backgrounds too
            outline(&mut out, w, h);
            (out, w, h)
        }
        _ => return None,
    };
    Some(CursorShape { rgba, px: w, py: h, w: w as f64, h: h as f64, hx, hy })
}

/// Gives opaque black pixels of a two-colour cursor a white edge where they meet transparency.
fn outline(px: &mut [u8], w: usize, h: usize) {
    let src = px.to_vec();
    let opaque = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && src[(y as usize * w + x as usize) * 4 + 3] != 0;
    for y in 0..h as isize {
        for x in 0..w as isize {
            if opaque(x, y) {
                continue;
            }
            if opaque(x - 1, y) || opaque(x + 1, y) || opaque(x, y - 1) || opaque(x, y + 1) {
                let i = (y as usize * w + x as usize) * 4;
                px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
}
