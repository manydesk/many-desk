// Platform specifics that don't fit the shared modules.
#[cfg(target_os = "linux")]
pub mod xinput;
pub mod perms;
pub mod system;
pub mod update;
#[cfg(windows)]
pub mod service;

/// Keeps the window titled `title` out of screen capture (the host's pointer overlay is
/// for the host only). Returns true once applied.
#[cfg(target_os = "macos")]
pub fn exclude_from_capture(title: &str) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::NSString;
    unsafe {
        let Some(cls) = AnyClass::get(c"NSApplication") else { return false };
        let app: *mut AnyObject = msg_send![cls, sharedApplication];
        if app.is_null() {
            return false;
        }
        let windows: Option<Retained<AnyObject>> = msg_send![app, windows];
        let Some(windows) = windows else { return false };
        let n: usize = msg_send![&*windows, count];
        for i in 0..n {
            let w: *mut AnyObject = msg_send![&*windows, objectAtIndex: i];
            let t: Option<Retained<NSString>> = msg_send![w, title];
            if t.is_some_and(|t| t.to_string() == title) {
                let _: () = msg_send![w, setSharingType: 0usize]; // NSWindowSharingNone
                let _: () = msg_send![w, setCollectionBehavior: 1usize | 16]; // all spaces, stationary
                return true;
            }
        }
    }
    false
}

#[cfg(windows)]
pub fn exclude_from_capture(title: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        !hwnd.is_null() && SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) != 0
    }
}

/// X11 has no capture exclusion; the overlay shows up in the shared picture there. The
/// phones have one window only.
#[cfg(any(target_os = "linux", mobile))]
pub fn exclude_from_capture(_title: &str) -> bool {
    true
}

/// The privacy cover: out of screen capture and above everything (menu bar and Dock
/// included on macOS). Returns true once applied.
#[cfg(target_os = "macos")]
pub fn cover_window(title: &str) -> bool {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::NSString;
    if !exclude_from_capture(title) {
        return false;
    }
    unsafe {
        let Some(cls) = AnyClass::get(c"NSApplication") else { return false };
        let app: *mut AnyObject = msg_send![cls, sharedApplication];
        let windows: Option<Retained<AnyObject>> = msg_send![app, windows];
        let Some(windows) = windows else { return false };
        let n: usize = msg_send![&*windows, count];
        for i in 0..n {
            let w: *mut AnyObject = msg_send![&*windows, objectAtIndex: i];
            let t: Option<Retained<NSString>> = msg_send![w, title];
            if t.is_some_and(|t| t.to_string() == title) {
                let _: () = msg_send![w, setLevel: 1000isize]; // NSScreenSaverWindowLevel
                // all spaces, stationary, over full-screen apps
                let _: () = msg_send![w, setCollectionBehavior: 1usize | 16 | 256];
                return true;
            }
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn cover_window(title: &str) -> bool {
    exclude_from_capture(title)
}

/// Opts out of App Nap for the process lifetime. A hidden window (covered, minimized, on
/// another Space) gets no redraws on macOS, so eframe runs our logic from a 250 ms timer,
/// and App Nap would defer that timer for tens of seconds: an incoming request or a live
/// session's input, chat and file-manager messages would go unanswered meanwhile.
#[cfg(target_os = "macos")]
pub fn keep_responsive() {
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    let reason = NSString::from_str("ManyDesk answers remote sessions in the background");
    let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(NSActivityOptions::UserInitiatedAllowingIdleSystemSleep, &reason);
    std::mem::forget(token); // never ended
}

/// Windows paints hidden windows directly and X11/Wayland time out missed redraws, so
/// the logic keeps running there (see eframe's native run loop).
#[cfg(not(target_os = "macos"))]
pub fn keep_responsive() {}
