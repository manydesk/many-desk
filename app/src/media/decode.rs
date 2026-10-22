// Viewer video: H.264 access units → RGBA for the GPU texture. Hardware first —
// VideoToolbox (macOS), Media Foundation with DXVA (Windows), VA-API through GStreamer
// (Linux) — and OpenH264 in software when there is none or it keeps failing.
use openh264::decoder::Decoder as H264Decoder;
use openh264::formats::YUVSource;

#[cfg(target_os = "linux")]
use super::gstdec::GstDecoder as HwDecoder;
#[cfg(target_os = "macos")]
use super::vtdec::VtDecoder as HwDecoder;
#[cfg(windows)]
use super::windec::MfDecoder as HwDecoder;

/// Hardware errors in a row before giving up on it for this session.
const HW_STRIKES: u32 = 3;

pub struct Decoder {
    hw: Option<HwDecoder>,
    strikes: u32,
    sw: Option<H264Decoder>,
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Decoder {
    pub fn new() -> Result<Self, String> {
        let hw = if std::env::var_os("MANYDESK_SOFTWARE_DECODE").is_some() { None } else { HwDecoder::new().map_err(|e| eprintln!("[decode] no hardware decoder: {e}")).ok() };
        let sw = if hw.is_none() { Some(H264Decoder::new().map_err(|e| e.to_string())?) } else { None };
        Ok(Decoder { hw, strikes: 0, sw, rgba: Vec::new(), width: 0, height: 0 })
    }

    /// What decodes the video, for the stats line.
    pub fn name(&self) -> &'static str {
        match &self.hw {
            Some(h) => h.name(),
            None => "OpenH264",
        }
    }

    /// The stream broke: start clean at the next keyframe.
    pub fn reset(&mut self) {
        if let Some(h) = &mut self.hw {
            h.reset();
        }
        if self.sw.is_some() {
            self.sw = H264Decoder::new().ok();
        }
    }

    /// Ok(true): `rgba` holds a new picture. Err: the stream is broken (ask for a keyframe).
    pub fn decode(&mut self, au: &[u8]) -> Result<bool, String> {
        if let Some(h) = &mut self.hw {
            match h.decode(au) {
                Ok(Some((rgba, w, h))) => {
                    self.strikes = 0;
                    self.rgba.clear();
                    self.rgba.extend_from_slice(rgba);
                    self.width = w;
                    self.height = h;
                    return Ok(true);
                }
                Ok(None) => return Ok(false),
                Err(e) => {
                    self.strikes += 1;
                    if self.strikes < HW_STRIKES {
                        return Err(e);
                    }
                    eprintln!("[decode] hardware decoder failing ({e}); OpenH264");
                    self.hw = None;
                    self.sw = Some(H264Decoder::new().map_err(|e| e.to_string())?);
                    return Err(e);
                }
            }
        }
        let dec = self.sw.as_mut().ok_or("no decoder")?;
        let Some(pic) = dec.decode(au).map_err(|e| e.to_string())? else { return Ok(false) };
        let (w, h) = pic.dimensions();
        self.rgba.resize(w * h * 4, 0);
        pic.write_rgba8(&mut self.rgba);
        self.width = w;
        self.height = h;
        Ok(true)
    }
}
