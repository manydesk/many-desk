// OS permissions a host needs: screen recording and input control (macOS asks for both;
// Windows and Linux/X11 need nothing).
#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
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
    #[cfg(not(target_os = "macos"))]
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
            "Privacy_Accessibility"
        };
        let _ = std::process::Command::new("open").arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}")).spawn();
    }
    let _ = screen;
}
