// Native media: screen capture, H.264 encoding (host) and decoding (viewer).
pub mod camera;
pub mod capture;
pub mod decode;
pub mod encode;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(windows)]
mod win;
#[cfg(windows)]
pub mod wincap;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod vtdec;
#[cfg(windows)]
mod windec;
#[cfg(target_os = "linux")]
mod gstdec;

/// OpenH264 slices of at most this many bytes: a big frame splits into several, which its
/// encoder threads code in parallel (a single slice per frame keeps it on one thread).
pub const SLICE_BYTES: u32 = 48_000;

/// OpenH264: a new bitrate and frame rate for a running encoder. A new encoder would start
/// with a keyframe — on a slow link a big one that stalls the picture for seconds, and
/// "auto" quality changes these every few seconds there. False if OpenH264 refused.
pub fn openh264_retune(e: &mut openh264::encoder::Encoder, fps: f64, bitrate: u32) -> bool {
    use openh264_sys2::{SBitrateInfo, ENCODER_OPTION_BITRATE, ENCODER_OPTION_FRAME_RATE, ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0, SPATIAL_LAYER_ALL};
    let set = |e: &mut openh264::encoder::Encoder, id, layer, b: i32| {
        let mut info = SBitrateInfo { iLayer: layer, iBitrate: b };
        unsafe { e.raw_api().set_option(id, &mut info as *mut SBitrateInfo as *mut std::ffi::c_void) == 0 }
    };
    let b = bitrate.min(i32::MAX as u32) as i32;
    // The single layer's ceiling was fixed at the first bitrate (a target above it is refused):
    // the ceilings first — the layer's and the whole stream's — then the targets.
    let m1 = set(e, ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0, b) & set(e, ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_ALL, b);
    let br = set(e, ENCODER_OPTION_BITRATE, SPATIAL_LAYER_0, b) & set(e, ENCODER_OPTION_BITRATE, SPATIAL_LAYER_ALL, b);
    let m2 = true;
    let mut f = fps as f32;
    let ok_fps = unsafe { e.raw_api().set_option(ENCODER_OPTION_FRAME_RATE, &mut f as *mut f32 as *mut std::ffi::c_void) == 0 };
    if std::env::var_os("MANYDESK_PERF").is_some() {
        eprintln!("[openh264] retune {fps} fps {bitrate} bit/s: max {m1} bitrate {br} max {m2} fps {ok_fps}");
    }
    br && ok_fps
}

/// The host's pointer, for a viewer that draws it itself (Session::cursor_apart).
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub struct CursorShape {
    /// RGBA, premultiplied (as drawn), `px` × `py` pixels
    pub rgba: Vec<u8>,
    pub px: usize,
    pub py: usize,
    /// size and hot spot in the display's pixels
    pub w: f64,
    pub h: f64,
    pub hx: f64,
    pub hy: f64,
}
