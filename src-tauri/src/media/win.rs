// Windows hardware H.264: the GPU vendor's Media Foundation encoder (Intel Quick Sync,
// NVIDIA NVENC, AMD AMF) as an asynchronous MFT, low-latency mode, no B-frames.
// Input NV12 from system memory; output is Annex-B. Without a hardware encoder the
// caller keeps using OpenH264.
use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};

pub struct MfEncoder {
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    credits: u32,
    provides_samples: bool,
    out_size: u32,
    width: usize,
    height: usize,
    frame: i64,
    fps: f64,
    nv12: Vec<u8>,
    pub name: String,
}

fn variant_u32(v: u32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    var
}

fn variant_bool(v: bool) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = windows::Win32::Foundation::VARIANT_BOOL(if v { -1 } else { 0 });
    }
    var
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl MfEncoder {
    pub fn new(width: usize, height: usize, fps: f64, bitrate: u32) -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut list,
                &mut count,
            )?;
            if count == 0 || list.is_null() {
                return Err(windows::core::Error::from_hresult(MF_E_TOPO_CODEC_NOT_FOUND));
            }
            let acts = std::slice::from_raw_parts(list, count as usize);
            let act = acts[0].clone().unwrap();
            let mut name = String::new();
            let mut wname = windows::core::PWSTR::null();
            let mut len = 0u32;
            if act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut wname, &mut len).is_ok() {
                name = wname.to_string().unwrap_or_default();
            }
            let mft: IMFTransform = act.ActivateObject()?;
            for a in acts {
                drop(a.clone());
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(list as *const _));

            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(width as u32, height as u32))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.round().max(1.0) as u32, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            mft.SetOutputType(0, &out, 0)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(width as u32, height as u32))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.round().max(1.0) as u32, 1))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &inp, 0)?;

            let codec: Option<ICodecAPI> = mft.cast().ok();
            if let Some(c) = &codec {
                let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &variant_bool(true));
                let _ = c.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
                let _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
                let _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32((fps * 10.0) as u32));
            }

            let info = mft.GetOutputStreamInfo(0)?;
            let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
            let events: IMFMediaEventGenerator = mft.cast()?;
            mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(MfEncoder {
                mft,
                events,
                codec,
                credits: 0,
                provides_samples,
                out_size: info.cbSize.max((width * height) as u32),
                width,
                height,
                frame: 0,
                fps,
                nv12: Vec::new(),
                name,
            })
        }
    }

    pub fn set_bitrate(&self, bitrate: u32) {
        if let Some(c) = &self.codec {
            unsafe {
                let _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
            }
        }
    }

    /// Handles queued encoder events; `wait`: block until the encoder wants input.
    fn pump(&mut self, out: &mut Vec<(Vec<u8>, bool)>, wait: bool) -> windows::core::Result<()> {
        loop {
            let flags = if wait && self.credits == 0 { MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0) } else { MF_EVENT_FLAG_NO_WAIT };
            let ev = match unsafe { self.events.GetEvent(flags) } {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(e),
            };
            let kind = unsafe { ev.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.credits += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                if let Some(f) = self.take_output()? {
                    out.push(f);
                }
            }
            if wait && self.credits > 0 {
                // Got what we waited for; drain the rest without blocking.
                return self.pump(out, false);
            }
        }
    }

    fn take_output(&mut self) -> windows::core::Result<Option<(Vec<u8>, bool)>> {
        unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(sample), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
            let sample = ManuallyDrop::take(&mut buf[0].pSample);
            drop(ManuallyDrop::take(&mut buf[0].pEvents));
            if let Err(e) = r {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(None);
                }
                return Err(e);
            }
            let Some(sample) = sample else { return Ok(None) };
            let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let mb = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            mb.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            mb.Unlock()?;
            Ok(Some((data, key)))
        }
    }

    /// Encodes one I420 frame; returns whatever access units are ready (usually this one).
    pub fn encode(&mut self, i420: &[u8], force_key: bool) -> windows::core::Result<Vec<(Vec<u8>, bool)>> {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w / 2, h / 2);
        // I420 → NV12 (interleave the chroma planes)
        self.nv12.resize(w * h + 2 * cw * ch, 0);
        self.nv12[..w * h].copy_from_slice(&i420[..w * h]);
        let (u, v) = i420[w * h..].split_at(cw * ch);
        for (i, uv) in self.nv12[w * h..].chunks_exact_mut(2).enumerate() {
            uv[0] = u[i];
            uv[1] = v[i];
        }
        let mut out = Vec::new();
        self.pump(&mut out, true)?;
        unsafe {
            if force_key {
                if let Some(c) = &self.codec {
                    let _ = c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
                }
            }
            let buf = MFCreateMemoryBuffer(self.nv12.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), ptr, self.nv12.len());
            buf.Unlock()?;
            buf.SetCurrentLength(self.nv12.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            let dur = (10_000_000.0 / self.fps) as i64;
            sample.SetSampleTime(self.frame * dur)?;
            sample.SetSampleDuration(dur)?;
            self.frame += 1;
            self.mft.ProcessInput(0, &sample, 0)?;
        }
        self.credits -= 1;
        self.pump(&mut out, false)?;
        Ok(out)
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

