// OS permissions a host needs: screen recording and input control (macOS asks for both,
// Android for control; Windows and Linux/X11 need nothing).
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFDictionaryCreate(a: *const c_void, keys: *const *const c_void, values: *const *const c_void, n: isize, kc: *const c_void, vc: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    /// Puts ManyDesk into the Accessibility list (unticked) and shows macOS's own prompt
    /// with its "Open System Settings" button. Without this call the app is not in the
    /// list at all, and the user would have to find and add it by hand.
    pub fn ask_accessibility() {
        unsafe {
            let (k, v) = ([kAXTrustedCheckOptionPrompt], [kCFBooleanTrue]);
            let d = CFDictionaryCreate(std::ptr::null(), k.as_ptr(), v.as_ptr(), 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            AXIsProcessTrustedWithOptions(d);
            CFRelease(d);
        }
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Perms {
    pub screen: bool,
    pub input: bool,
}

pub fn status() -> Perms {
    #[cfg(target_os = "macos")]
    unsafe {
        Perms { screen: mac::CGPreflightScreenCaptureAccess(), input: mac::AXIsProcessTrusted() }
    }
    // Android: the screen is asked for when a session starts (MediaProjection); control
    // needs the accessibility service on.
    #[cfg(target_os = "android")]
    return Perms { screen: true, input: crate::android::input_ready() };
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    Perms { screen: true, input: true }
}

/// Asks the OS (system prompt / opens the right Settings pane).
pub fn request(screen: bool) {
    #[cfg(target_os = "macos")]
    {
        let anchor = if screen {
            unsafe { mac::CGRequestScreenCaptureAccess() };
            "Privacy_ScreenCapture"
        } else {
            mac::ask_accessibility();
            "Privacy_Accessibility"
        };
        let _ = std::process::Command::new("open").arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}")).spawn();
    }
    #[cfg(target_os = "android")]
    if !screen {
        crate::android::open_input_settings();
    }
    let _ = screen;
}
