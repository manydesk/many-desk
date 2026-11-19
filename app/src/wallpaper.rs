//! This computer's desktop wallpaper as a small JPEG. A host sends it to the viewer when a
//! session starts, and the viewer's recent-sessions tile shows it (as AnyDesk does) instead
//! of a picture of the screen. None when it cannot be read: the viewer then keeps a frame
//! of the session, as before.

use image::DynamicImage;

/// Wide enough for a tile on a high-density screen, small enough for the control stream.
const MAX_W: u32 = 640;

pub fn jpeg() -> Option<Vec<u8>> {
    let img = load()?;
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    encode(&img)
}

fn encode(img: &DynamicImage) -> Option<Vec<u8>> {
    let img = if img.width() > MAX_W { img.thumbnail(MAX_W, (MAX_W as u64 * img.height() as u64 / img.width() as u64).max(1) as u32) } else { img.clone() };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode_image(&img.to_rgb8()).ok()?;
    Some(out)
}

/// A wallpaper of one colour (no picture set): a small plain image.
#[cfg(any(windows, target_os = "linux"))]
fn solid(r: u8, g: u8, b: u8) -> DynamicImage {
    DynamicImage::ImageRgb8(image::RgbImage::from_pixel(160, 90, image::Rgb([r, g, b])))
}

#[cfg(target_os = "macos")]
fn load() -> Option<DynamicImage> {
    use objc2::rc::autoreleasepool;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let path = autoreleasepool(|_| unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if ws.is_null() || screen.is_null() {
            return None;
        }
        let url: *mut AnyObject = msg_send![ws, desktopImageURLForScreen: screen];
        if url.is_null() {
            return None;
        }
        let path: *mut AnyObject = msg_send![url, path];
        if path.is_null() {
            return None;
        }
        let s: *const std::ffi::c_char = msg_send![path, UTF8String];
        (!s.is_null()).then(|| std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned())
    })?;
    // The system wallpapers are HEIC (and Aerial ones a still of the video): sips reads them all.
    let out = std::env::temp_dir().join(format!("manydesk-wallpaper-{}.jpg", std::process::id()));
    let ok = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg", "-Z", &(MAX_W * 2).to_string()])
        .arg(&path)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let img = ok.then(|| image::open(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    img
}

#[cfg(windows)]
fn load() -> Option<DynamicImage> {
    use windows_sys::Win32::Graphics::Gdi::GetSysColor;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETDESKWALLPAPER};
    let mut buf = [0u16; 1024];
    let ok = unsafe { SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, buf.as_mut_ptr().cast(), 0) } != 0;
    let path = if ok { String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]) } else { String::new() };
    if !path.is_empty() {
        if let Ok(img) = image::open(&path) {
            return Some(img);
        }
        // Windows' own copy of it (also for a slideshow, or a picture it can no longer find)
        let copy = std::env::var_os("APPDATA").map(|a| std::path::Path::new(&a).join(r"Microsoft\Windows\Themes\TranscodedWallpaper"));
        return copy.and_then(|c| std::fs::read(c).ok()).and_then(|d| image::load_from_memory(&d).ok());
    }
    // no picture: the background colour (COLOR_DESKTOP, 0x00BBGGRR)
    let c = unsafe { GetSysColor(1) };
    Some(solid(c as u8, (c >> 8) as u8, (c >> 16) as u8))
}

#[cfg(target_os = "linux")]
fn load() -> Option<DynamicImage> {
    let get = |key: &str| {
        let o = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.background", key]).output().ok()?;
        let v = String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string();
        (o.status.success() && !v.is_empty()).then_some(v)
    };
    let dark = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "color-scheme"]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("dark"));
    let uri = (if dark { get("picture-uri-dark") } else { None }).or_else(|| get("picture-uri"));
    if let Some(path) = uri.as_deref().and_then(|u| u.strip_prefix("file://")).map(percent_decode) {
        if let Ok(img) = image::open(&path) {
            return Some(img);
        }
    }
    // no picture: GNOME's background colour ("#rrggbb")
    let c = get("primary-color")?;
    let n = u32::from_str_radix(c.trim_start_matches('#').get(..6)?, 16).ok()?;
    Some(solid((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

#[cfg(target_os = "linux")]
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os = "android")]
fn load() -> Option<DynamicImage> {
    image::load_from_memory(&crate::android::wallpaper_jpeg(MAX_W as i32)?).ok()
}

#[cfg(target_os = "ios")]
fn load() -> Option<DynamicImage> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn encode_shrinks_wide_pictures() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1920, 1080, image::Rgb([10, 120, 200])));
        let j = super::encode(&img).unwrap();
        let back = image::load_from_memory(&j).unwrap();
        assert_eq!((back.width(), back.height()), (640, 360));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn percent_decoding() {
        assert_eq!(super::percent_decode("/home/a/My%20Pics/x.jpg"), "/home/a/My Pics/x.jpg");
        assert_eq!(super::percent_decode("/a%2"), "/a%2");
    }
}
