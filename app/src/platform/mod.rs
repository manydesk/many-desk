// Platform specifics that don't fit the shared modules.
#[cfg(target_os = "linux")]
pub mod xinput;
pub mod display;
pub mod vdisplay;
pub mod perms;
pub mod system;
pub mod update;
pub mod tray;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod instance;

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

/// While someone is connected here, the display stays on (as AnyDesk does). Left to its
/// idle timer it went dark in the middle of a session: on macOS the capture then stops,
/// the viewer sees a frozen picture, and the fresh stream when it woke was read as a slow
/// link (resolution and frame rate fell for minutes). Called every frame; acts on changes.
pub fn hold_display(on: bool) {
    use std::cell::Cell;
    thread_local! { static HELD: Cell<bool> = const { Cell::new(false) }; }
    if HELD.with(|h| h.replace(on)) == on {
        return;
    }
    display_hold(on);
}

#[cfg(target_os = "macos")]
fn display_hold(on: bool) {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    use std::cell::RefCell;
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionDeclareUserActivity(name: *const objc2_core_foundation::CFString, user_type: u32, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }
    thread_local! {
        static TOKEN: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
        static ACTIVITY: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let info = NSProcessInfo::processInfo();
    TOKEN.with(|t| {
        if let Some(token) = t.borrow_mut().take() {
            unsafe { info.endActivity(&token) };
        }
        if on {
            let reason = NSString::from_str("Someone is connected to this computer with ManyDesk");
            let opts = NSActivityOptions::UserInitiated | NSActivityOptions::IdleDisplaySleepDisabled;
            *t.borrow_mut() = Some(info.beginActivityWithOptions_reason(opts, &reason));
        }
    });
    // A display that went dark before anyone connected is woken (as a key press would):
    // keeping it on does not turn it back on, and with it off there is nothing to capture.
    let old = ACTIVITY.with(|a| a.replace(0));
    if old != 0 {
        unsafe { IOPMAssertionRelease(old) };
    }
    if on {
        let name = objc2_core_foundation::CFString::from_str("ManyDesk: a remote session started");
        let mut id = 0u32;
        // kIOPMUserActiveLocal
        if unsafe { IOPMAssertionDeclareUserActivity(&*name, 0, &mut id) } == 0 {
            ACTIVITY.with(|a| a.set(id));
        }
    }
}

#[cfg(windows)]
fn display_hold(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
    // per thread: this one (the UI's) lives as long as the app
    unsafe { SetThreadExecutionState(if on { ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS }) };
}

#[cfg(not(any(target_os = "macos", windows)))]
fn display_hold(_on: bool) {}
