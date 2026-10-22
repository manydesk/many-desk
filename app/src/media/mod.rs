// Native media: screen capture, H.264 encoding (host) and decoding (viewer).
pub mod capture;
pub mod decode;
pub mod encode;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
mod vtdec;
#[cfg(windows)]
mod windec;
#[cfg(target_os = "linux")]
mod gstdec;
