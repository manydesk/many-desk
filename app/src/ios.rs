// iOS glue: a viewer only — iOS lets no app capture the whole screen or control the
// phone, so this side never hosts. The app starts in ios/main.m, which calls
// `manydesk_main` (lib.rs); the soft keyboard follows egui's text fields through winit.
use std::sync::Mutex;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::NSString;

fn pasteboard() -> Option<*mut AnyObject> {
    let class = AnyClass::get(c"UIPasteboard")?;
    let pb: *mut AnyObject = unsafe { msg_send![class, generalPasteboard] };
    (!pb.is_null()).then_some(pb)
}

pub fn set_clipboard_text(text: &str) {
    let Some(pb) = pasteboard() else { return };
    let s = NSString::from_str(text);
    unsafe {
        let _: () = msg_send![pb, setString: &*s];
    }
}

/// The pasteboard's text. iOS tells the user every time an app reads it, so it is read
/// only after it changed (changeCount), not on every poll.
pub fn clipboard_text() -> Option<String> {
    static LAST: Mutex<(isize, Option<String>)> = Mutex::new((-1, None));
    let pb = pasteboard()?;
    let count: isize = unsafe { msg_send![pb, changeCount] };
    let mut last = LAST.lock().ok()?;
    if last.0 != count {
        let has: bool = unsafe { msg_send![pb, hasStrings] };
        let text: Option<Retained<NSString>> = if has { unsafe { msg_send![pb, string] } } else { None };
        *last = (count, text.map(|t| t.to_string()));
    }
    last.1.clone()
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EdgeInsets {
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
}

unsafe impl objc2::Encode for EdgeInsets {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("UIEdgeInsets", &[<f64 as objc2::Encode>::ENCODING; 4]);
}

/// The notch / Dynamic Island and the home indicator, in points: (top, bottom).
pub fn insets() -> (f32, f32) {
    let Some(class) = AnyClass::get(c"UIApplication") else { return (0.0, 0.0) };
    unsafe {
        let app: *mut AnyObject = msg_send![class, sharedApplication];
        if app.is_null() {
            return (0.0, 0.0);
        }
        let windows: *mut AnyObject = msg_send![app, windows];
        let window: *mut AnyObject = if windows.is_null() { std::ptr::null_mut() } else { msg_send![windows, firstObject] };
        if window.is_null() {
            return (0.0, 0.0);
        }
        let i: EdgeInsets = msg_send![window, safeAreaInsets];
        (i.top as f32, i.bottom as f32)
    }
}

/// winit shows the keyboard when egui asks for text input (set_ime_allowed).
pub fn sync_keyboard(_want: bool) {}
