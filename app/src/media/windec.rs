// Windows viewer video: Microsoft's H.264 decoder MFT with a D3D11 device manager, so
// it decodes on the GPU (DXVA). The NV12 picture is read back through IMF2DBuffer and
// converted to RGBA. Without a usable D3D11 device the caller falls back to OpenH264.
use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, ID3D11Device, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

pub struct MfDecoder {
    mft: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    provides_samples: bool,
    out_size: u32,
    /// coded size (NV12 planes) and visible size
    coded: (usize, usize),
    visible: (usize, usize),
    rgba: Vec<u8>,
}

unsafe impl Send for MfDecoder {}

fn unpack(v: u64) -> (usize, usize) {
    ((v >> 32) as usize, (v & 0xffff_ffff) as usize)
}

impl MfDecoder {
    pub fn new() -> Result<Self, String> {
        Self::open().map_err(|e| e.to_string())
    }

    fn open() -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
            let device = device.ok_or_else(|| windows::core::Error::from_hresult(MF_E_UNSUPPORTED_D3D_TYPE))?;
            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| windows::core::Error::from_hresult(MF_E_UNSUPPORTED_D3D_TYPE))?;
            manager.ResetDevice(&device, token)?;

            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            let attrs = mft.GetAttributes()?;
            // Hardware only: without D3D11 support in the decoder this fails and OpenH264 is used.
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(windows::core::Error::from_hresult(MF_E_UNSUPPORTED_D3D_TYPE));
            }
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &inp, 0)?;
            let mut me = MfDecoder { mft, _manager: manager, provides_samples: true, out_size: 0, coded: (0, 0), visible: (0, 0), rgba: Vec::new() };
            me.pick_output()?;
            me.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            me.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(me)
        }
    }

    pub fn name(&self) -> &'static str {
        "Media Foundation (DXVA)"
    }

    /// NV12 output; called at start and whenever the stream format changes.
    fn pick_output(&mut self) -> windows::core::Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    break;
                }
                i += 1;
            }
            let t = self.mft.GetOutputCurrentType(0)?;
            if let Ok(v) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                self.coded = unpack(v);
                self.visible = self.coded;
            }
            // The visible area (e.g. 1080 of a 1088-line coded picture).
            let mut ap = MFVideoArea::default();
            let p = &mut ap as *mut MFVideoArea as *mut u8;
            let mut got = 0u32;
            if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, std::slice::from_raw_parts_mut(p, std::mem::size_of::<MFVideoArea>()), Some(&mut got)).is_ok() && got > 0 {
                let (w, h) = (ap.Area.cx as usize, ap.Area.cy as usize);
                if w > 0 && h > 0 && w <= self.coded.0 && h <= self.coded.1 {
                    self.visible = (w, h);
                }
            }
            let info = self.mft.GetOutputStreamInfo(0)?;
            self.provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
            self.out_size = info.cbSize;
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Option<(&[u8], usize, usize)>, String> {
        self.decode_inner(au).map_err(|e| e.to_string())?;
        Ok(if self.rgba.is_empty() { None } else { Some((self.rgba.as_slice(), self.visible.0, self.visible.1)) })
    }

    fn decode_inner(&mut self, au: &[u8]) -> windows::core::Result<()> {
        self.rgba.clear();
        unsafe {
            let buf = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), ptr, au.len());
            buf.Unlock()?;
            buf.SetCurrentLength(au.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime(0)?;
            self.mft.ProcessInput(0, &sample, 0)?;
            // Low-latency mode: the picture for this access unit is ready now.
            loop {
                let out = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.out_size.max((self.coded.0 * self.coded.1 * 3 / 2) as u32))?)?;
                    Some(s)
                };
                let mut db = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(out), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                let mut status = 0u32;
                let r = self.mft.ProcessOutput(0, &mut db, &mut status);
                let got = ManuallyDrop::take(&mut db[0].pSample);
                drop(ManuallyDrop::take(&mut db[0].pEvents));
                match r {
                    Ok(()) => {
                        if let Some(s) = got {
                            self.read(&s)?;
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.pick_output()?,
                    Err(e) => return Err(e),
                }
            }
        }
    }

    /// NV12 sample (GPU or system memory) → RGBA.
    unsafe fn read(&mut self, s: &IMFSample) -> windows::core::Result<()> {
        let mb = s.GetBufferByIndex(0)?;
        let (cw, ch) = self.coded;
        let (w, h) = self.visible;
        if let Ok(b2) = mb.cast::<IMF2DBuffer>() {
            let mut p = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut p, &mut pitch)?;
            let pitch = pitch.unsigned_abs() as usize;
            let all = std::slice::from_raw_parts(p, pitch * ch * 3 / 2);
            nv12_to_rgba(all, pitch, ch, w, h, &mut self.rgba);
            b2.Unlock2D()?;
        } else {
            let mut p = std::ptr::null_mut();
            let mut len = 0u32;
            mb.Lock(&mut p, None, Some(&mut len))?;
            let all = std::slice::from_raw_parts(p, len as usize);
            if all.len() >= cw * ch * 3 / 2 {
                nv12_to_rgba(all, cw, ch, w, h, &mut self.rgba);
            }
            mb.Unlock()?;
        }
        Ok(())
    }
}

/// NV12 (Y plane `pitch`×`rows`, then interleaved UV) → RGBA, BT.601 limited range.
pub fn nv12_to_rgba(src: &[u8], pitch: usize, rows: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 0);
    let (y_plane, uv_plane) = src.split_at(pitch * rows);
    for y in 0..h {
        let yr = &y_plane[y * pitch..y * pitch + w];
        let uvr = &uv_plane[(y / 2) * pitch..];
        let o = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let c = (yr[x] as i32 - 16) * 298;
            let d = uvr[x & !1] as i32 - 128;
            let e = uvr[(x & !1) + 1] as i32 - 128;
            let px = &mut o[x * 4..x * 4 + 4];
            px[0] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            px[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            px[2] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            px[3] = 255;
        }
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}
