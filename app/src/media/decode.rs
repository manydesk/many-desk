// Viewer video: H.264 access units → pictures for the GPU. Hardware first — VideoToolbox
// (macOS, iOS), Media Foundation with DXVA (Windows), VA-API through GStreamer (Linux) —
// and OpenH264 in software when there is none or it keeps failing (and on Android, for
// now). Pictures stay in the decoder's YCbCr layout; the stream's SPS says how its
// YCbCr maps to RGB (BT.601 or BT.709, limited or full range), and the renderer does it.
use openh264::decoder::Decoder as H264Decoder;
use openh264::formats::YUVSource;

use crate::video::{Color, Picture, Planes};

#[cfg(target_os = "linux")]
use super::gstdec::GstDecoder as HwDecoder;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use super::vtdec::VtDecoder as HwDecoder;
#[cfg(target_os = "android")]
use NoHardware as HwDecoder;

/// Android: no hardware path yet (MediaCodec), OpenH264 does it.
#[cfg(target_os = "android")]
struct NoHardware;

#[cfg(target_os = "android")]
impl NoHardware {
    fn new() -> Result<Self, String> {
        Err("not on Android yet".into())
    }
    fn name(&self) -> &'static str {
        "none"
    }
    fn reset(&mut self) {}
    fn decode(&mut self, _au: &[u8], _buf: Vec<u8>, _color: Color) -> Result<Option<Picture>, String> {
        Err("none".into())
    }
}
#[cfg(windows)]
use super::windec::MfDecoder as HwDecoder;

/// Hardware errors in a row before giving up on it for this session.
const HW_STRIKES: u32 = 3;

pub struct Decoder {
    hw: Option<HwDecoder>,
    strikes: u32,
    sw: Option<H264Decoder>,
    /// from the stream's latest SPS
    color: Color,
    seen_sps: bool,
}

impl Decoder {
    pub fn new() -> Result<Self, String> {
        let hw = if std::env::var_os("MANYDESK_SOFTWARE_DECODE").is_some() { None } else { HwDecoder::new().map_err(|e| eprintln!("[decode] no hardware decoder: {e}")).ok() };
        let sw = if hw.is_none() { Some(H264Decoder::new().map_err(|e| e.to_string())?) } else { None };
        Ok(Decoder { hw, strikes: 0, sw, color: Color::default(), seen_sps: false })
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

    /// Ok(Some): a new picture, written into `buf` (a spare buffer, reused). Err: the
    /// stream is broken (ask for a keyframe).
    pub fn decode(&mut self, au: &[u8], buf: Vec<u8>) -> Result<Option<Picture>, String> {
        if let Some(sps) = nals(au).into_iter().find(|n| n[0] & 0x1f == 7) {
            if let Some(c) = sps_color(sps).filter(|c| *c != self.color || !self.seen_sps) {
                eprintln!("[decode] stream colour: {} {} range", if c.bt709 { "BT.709" } else { "BT.601" }, if c.full { "full" } else { "limited" });
                self.color = c;
                self.seen_sps = true;
            }
        }
        if let Some(h) = &mut self.hw {
            match h.decode(au, buf, self.color) {
                Ok(p) => {
                    self.strikes = 0;
                    return Ok(p);
                }
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
        let Some(pic) = dec.decode(au).map_err(|e| e.to_string())? else { return Ok(None) };
        let (w, h) = pic.dimensions();
        let (ys, us, vs) = pic.strides();
        // the planes as OpenH264 has them (its strides), one copy into our buffer
        let (y, u, v) = (pic.y(), pic.u(), pic.v());
        let (ch, cw) = (h.div_ceil(2), w.div_ceil(2));
        let mut data = buf;
        data.clear();
        data.reserve(w * h + 2 * cw * ch);
        for r in 0..h {
            data.extend_from_slice(&y[r * ys..r * ys + w]);
        }
        let u_offset = data.len();
        for r in 0..ch {
            data.extend_from_slice(&u[r * us..r * us + cw]);
        }
        let v_offset = data.len();
        for r in 0..ch {
            data.extend_from_slice(&v[r * vs..r * vs + cw]);
        }
        Ok(Some(Picture { width: w, height: h, planes: Planes::I420 { y_stride: w, u_offset, v_offset, c_stride: cw }, data, color: self.color }))
    }
}

/// NAL units of an Annex-B buffer (start codes removed).
pub fn nals(b: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = vec![];
    for (k, &(_, s)) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map(|n| n.0).unwrap_or(b.len());
        while e > s && b[e - 1] == 0 {
            e -= 1; // the leading zero of a 4-byte start code
        }
        if e > s {
            out.push(&b[s..e]);
        }
    }
    out
}

/// Reads an H.264 RBSP bit by bit (emulation-prevention bytes already removed).
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.pos / 8)?;
        let v = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |acc, _| Some((acc << 1) | self.bit()?))
    }
    /// ue(v)
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    /// se(v)
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { ((k + 1) / 2) as i32 } else { -(k / 2) as i32 })
    }
}

/// The colour signalling in an SPS NAL unit (type 7): the VUI's matrix coefficients and
/// full-range flag. None when the SPS cannot be read; an SPS without a VUI or without a
/// colour description means BT.601 limited, what encoders write by default.
pub fn sps_color(nal: &[u8]) -> Option<Color> {
    // RBSP: 00 00 03 → 00 00
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal.get(1..)? {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    let r = &mut Bits { b: &rbsp, pos: 0 };
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags, level
    r.ue()?; // seq_parameter_set_id
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?; // bit depth luma
        r.ue()?; // bit depth chroma
        r.bit()?; // qpprime_y_zero_transform_bypass
        if r.bit()? == 1 {
            // scaling lists: skip their delta values
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_allowed
    r.ue()?; // width in macroblocks
    r.ue()?; // height in map units
    if r.bit()? == 0 {
        r.bit()?; // mb_adaptive_frame_field
    }
    r.bit()?; // direct_8x8_inference
    if r.bit()? == 1 {
        for _ in 0..4 {
            r.ue()?; // cropping
        }
    }
    let default = Color { bt709: false, full: false };
    if r.bit()? == 0 {
        return Some(default); // no VUI
    }
    if r.bit()? == 1 && r.bits(8)? == 255 {
        r.bits(32)?; // sample aspect ratio
    }
    if r.bit()? == 1 {
        r.bit()?; // overscan_appropriate
    }
    if r.bit()? == 0 {
        return Some(default); // no video signal type
    }
    r.bits(3)?; // video_format
    let full = r.bit()? == 1;
    if r.bit()? == 0 {
        return Some(Color { bt709: false, full });
    }
    r.bits(8)?; // colour primaries
    r.bits(8)?; // transfer characteristics
    let matrix = r.bits(8)?;
    // 1: BT.709; 5, 6: BT.601; 2: unspecified (the BT.601 default)
    Some(Color { bt709: matrix == 1, full })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an SPS bit by bit (exp-Golomb like the encoders), with emulation prevention.
    struct W(Vec<bool>);
    impl W {
        fn bits(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.0.push((v >> i) & 1 == 1);
            }
        }
        fn ue(&mut self, v: u32) {
            let x = v + 1;
            let len = 32 - x.leading_zeros();
            self.bits(0, len - 1);
            self.bits(x, len);
        }
        fn nal(mut self) -> Vec<u8> {
            self.0.push(true); // rbsp stop bit
            while self.0.len() % 8 != 0 {
                self.0.push(false);
            }
            let bytes: Vec<u8> = self.0.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8)).collect();
            let mut out = vec![0x67];
            let mut zeros = 0;
            for b in bytes {
                if zeros >= 2 && b <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                zeros = if b == 0 { zeros + 1 } else { 0 };
                out.push(b);
            }
            out
        }
    }

    fn sps(profile: u32, vui: Option<(bool, Option<u32>)>) -> Vec<u8> {
        let mut w = W(vec![]);
        w.bits(profile, 8);
        w.bits(0, 8);
        w.bits(40, 8);
        w.ue(0);
        if profile == 100 {
            w.ue(1); // 4:2:0
            w.ue(0);
            w.ue(0);
            w.bits(0, 1);
            w.bits(0, 1); // no scaling matrix
        }
        w.ue(0);
        w.ue(0); // poc type 0
        w.ue(4);
        w.ue(1);
        w.bits(0, 1);
        w.ue(188); // 3024 px wide
        w.ue(118); // 1904 px tall
        w.bits(1, 1); // frame_mbs_only
        w.bits(1, 1);
        w.bits(1, 1); // cropping
        for v in [0, 0, 0, 3] {
            w.ue(v);
        }
        match vui {
            None => w.bits(0, 1),
            Some((full, matrix)) => {
                w.bits(1, 1);
                w.bits(0, 1); // no aspect ratio
                w.bits(0, 1); // no overscan
                w.bits(1, 1); // video signal type
                w.bits(5, 3);
                w.bits(full as u32, 1);
                match matrix {
                    None => w.bits(0, 1),
                    Some(m) => {
                        w.bits(1, 1);
                        w.bits(m, 8);
                        w.bits(m, 8);
                        w.bits(m, 8);
                    }
                }
                w.bits(0, 1); // no chroma loc ... (rest of the VUI is not read)
            }
        }
        w.nal()
    }

    #[test]
    fn reads_the_colour_of_an_sps() {
        assert_eq!(sps_color(&sps(100, None)), Some(Color { bt709: false, full: false }));
        assert_eq!(sps_color(&sps(66, Some((false, None)))), Some(Color { bt709: false, full: false }));
        assert_eq!(sps_color(&sps(100, Some((false, Some(1))))), Some(Color { bt709: true, full: false }));
        assert_eq!(sps_color(&sps(100, Some((true, Some(6))))), Some(Color { bt709: false, full: true }));
        assert_eq!(sps_color(&sps(77, Some((true, Some(1))))), Some(Color { bt709: true, full: true }));
        // too short to read: no guess
        assert_eq!(sps_color(&[0x67, 100]), None);
    }

    #[test]
    fn splits_annex_b() {
        let b = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let n = nals(&b);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn yuv_converts_like_the_reference() {
        // a grey and a saturated red, BT.601 limited: 16..235 luma, 128 neutral chroma
        let (w, h) = (2, 2);
        let mut data = vec![126, 82, 126, 82]; // Y
        data.extend_from_slice(&[128]); // Cb (one sample for 2x2)
        data.extend_from_slice(&[128]); // Cr
        let pic = Picture { width: w, height: h, planes: Planes::I420 { y_stride: 2, u_offset: 4, v_offset: 5, c_stride: 1 }, data, color: Color::default() };
        let rgba = pic.rgba();
        assert_eq!(&rgba[0..4], &[128, 128, 128, 255]);
        // NV12 of pure red (BT.601 limited: Y 81, Cb 90, Cr 240)
        let pic = Picture { width: 2, height: 2, planes: Planes::Nv12 { y_stride: 2, uv_offset: 4, uv_stride: 2 }, data: vec![81, 81, 81, 81, 90, 240], color: Color::default() };
        let px = &pic.rgba()[0..4];
        assert!(px[0] >= 250 && px[1] <= 3 && px[2] <= 3, "{px:?}");
    }
}
