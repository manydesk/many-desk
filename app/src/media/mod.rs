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
