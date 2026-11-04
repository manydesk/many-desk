// The resolution of a display: what it offers and switching to one (the viewer picks it,
// as in AnyDesk's and RustDesk's "Display → Resolution"). A changed display goes back to
// what it was when the session ends (core.rs: restore_modes); on macOS also when ManyDesk
// quits (the change is made for this session of the app only).
//
// Display ids are those of media::capture::Screen (CGDirectDisplayID on macOS, HMONITOR on
// Windows). Linux and the phones: no modes (the menu says so).

/// One mode: the size the desktop is laid out in (points on a Retina Mac), its refresh.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub hz: u32,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Mode;
    use std::ffi::c_void;

    type CFArrayRef = *const c_void;
    type ModeRef = *const c_void;
    type ConfigRef = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGDisplayCopyAllDisplayModes(display: u32, options: *const c_void) -> CFArrayRef;
        fn CGDisplayCopyDisplayMode(display: u32) -> ModeRef;
        fn CGDisplayModeGetWidth(mode: ModeRef) -> usize;
        fn CGDisplayModeGetHeight(mode: ModeRef) -> usize;
        fn CGDisplayModeGetPixelWidth(mode: ModeRef) -> usize;
        fn CGDisplayModeGetRefreshRate(mode: ModeRef) -> f64;
        fn CGDisplayModeIsUsableForDesktopGUI(mode: ModeRef) -> bool;
        fn CGDisplayModeRelease(mode: ModeRef);
        fn CGBeginDisplayConfiguration(config: *mut ConfigRef) -> i32;
        fn CGConfigureDisplayWithDisplayMode(config: ConfigRef, display: u32, mode: ModeRef, options: *const c_void) -> i32;
        fn CGCompleteDisplayConfiguration(config: ConfigRef, option: u32) -> i32;
        fn CGCancelDisplayConfiguration(config: ConfigRef) -> i32;
        static kCGDisplayShowDuplicateLowResolutionModes: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, i: isize) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        fn CFDictionaryCreate(alloc: *const c_void, keys: *const *const c_void, values: *const *const c_void, n: isize, kcb: *const c_void, vcb: *const c_void) -> *const c_void;
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
    }

    /// kCGConfigureForSession: undone when this app quits
    const FOR_SESSION: u32 = 1;

    fn mode_of(m: ModeRef) -> Mode {
        unsafe { Mode { w: CGDisplayModeGetWidth(m) as u32, h: CGDisplayModeGetHeight(m) as u32, hz: CGDisplayModeGetRefreshRate(m).round() as u32 } }
    }

    /// Calls `f` with each usable mode (the array is released after).
    fn each(display: u32, mut f: impl FnMut(ModeRef) -> bool) {
        unsafe {
            // with the HiDPI ("looks like") modes a Retina display normally runs in
            let (k, v) = (kCGDisplayShowDuplicateLowResolutionModes, kCFBooleanTrue);
            let opts = CFDictionaryCreate(std::ptr::null(), &k, &v, 1, &kCFTypeDictionaryKeyCallBacks as *const u8 as *const c_void, &kCFTypeDictionaryValueCallBacks as *const u8 as *const c_void);
            let all = CGDisplayCopyAllDisplayModes(display, opts);
            if !opts.is_null() {
                CFRelease(opts);
            }
            if all.is_null() {
                return;
            }
            for i in 0..CFArrayGetCount(all) {
                let m = CFArrayGetValueAtIndex(all, i);
                if CGDisplayModeIsUsableForDesktopGUI(m) && !f(m) {
                    break;
                }
            }
            CFRelease(all);
        }
    }

    pub fn current(display: u32) -> Option<Mode> {
        unsafe {
            let m = CGDisplayCopyDisplayMode(display);
            if m.is_null() {
                return None;
            }
            let out = mode_of(m);
            CGDisplayModeRelease(m);
            Some(out)
        }
    }

    pub fn modes(display: u32) -> Vec<Mode> {
        let mut out = Vec::new();
        each(display, |m| {
            out.push(mode_of(m));
            true
        });
        out
    }

    pub fn set(display: u32, want: Mode) -> Result<(), String> {
        // the same size: the sharper (HiDPI) one, then the refresh asked for
        let mut best: Option<(ModeRef, (bool, bool, usize))> = None;
        let mut found = false;
        each(display, |m| {
            let mo = mode_of(m);
            if mo.w == want.w && mo.h == want.h {
                let score = (mo.hz == want.hz, unsafe { CGDisplayModeGetPixelWidth(m) } > mo.w as usize, unsafe { CGDisplayModeGetPixelWidth(m) });
                if best.is_none_or(|(_, s)| score > s) {
                    best = Some((m, score));
                }
                found = true;
            }
            true
        });
        // (the modes live in the array `each` released: look the winner up again)
        let Some((_, score)) = best.filter(|_| found) else { return Err("no such mode".into()) };
        let mut result = Err("no such mode".to_string());
        each(display, |m| {
            let mo = mode_of(m);
            let s = (mo.hz == want.hz, unsafe { CGDisplayModeGetPixelWidth(m) } > mo.w as usize, unsafe { CGDisplayModeGetPixelWidth(m) });
            if mo.w == want.w && mo.h == want.h && s == score {
                result = unsafe {
                    let mut cfg: ConfigRef = std::ptr::null_mut();
                    if CGBeginDisplayConfiguration(&mut cfg) != 0 {
                        Err("CGBeginDisplayConfiguration".to_string())
                    } else if CGConfigureDisplayWithDisplayMode(cfg, display, m, std::ptr::null()) != 0 {
                        CGCancelDisplayConfiguration(cfg);
                        Err("CGConfigureDisplayWithDisplayMode".to_string())
                    } else if CGCompleteDisplayConfiguration(cfg, FOR_SESSION) != 0 {
                        Err("CGCompleteDisplayConfiguration".to_string())
                    } else {
                        Ok(())
                    }
                };
                return false;
            }
            true
        });
        result
    }
}

#[cfg(windows)]
mod imp {
    use super::Mode;
    use windows_sys::Win32::Graphics::Gdi::{ChangeDisplaySettingsExW, EnumDisplaySettingsExW, GetMonitorInfoW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, HMONITOR, MONITORINFOEXW};

    /// "\\.\DISPLAY1" of a monitor (NUL-terminated).
    fn device(display: u32) -> Option<[u16; 32]> {
        unsafe {
            let mut mi: MONITORINFOEXW = std::mem::zeroed();
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            (GetMonitorInfoW(display as usize as HMONITOR, &mut mi as *mut MONITORINFOEXW as *mut _) != 0).then_some(mi.szDevice)
        }
    }

    fn dm() -> DEVMODEW {
        let mut d: DEVMODEW = unsafe { std::mem::zeroed() };
        d.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        d
    }

    pub fn current(display: u32) -> Option<Mode> {
        let dev = device(display)?;
        let mut d = dm();
        (unsafe { EnumDisplaySettingsExW(dev.as_ptr(), ENUM_CURRENT_SETTINGS, &mut d, 0) } != 0).then(|| Mode { w: d.dmPelsWidth, h: d.dmPelsHeight, hz: d.dmDisplayFrequency })
    }

    pub fn modes(display: u32) -> Vec<Mode> {
        let Some(dev) = device(display) else { return Vec::new() };
        let mut out = Vec::new();
        let mut d = dm();
        let mut i = 0;
        while unsafe { EnumDisplaySettingsExW(dev.as_ptr(), i, &mut d, 0) } != 0 {
            if d.dmBitsPerPel == 32 {
                out.push(Mode { w: d.dmPelsWidth, h: d.dmPelsHeight, hz: d.dmDisplayFrequency });
            }
            i += 1;
        }
        out
    }

    pub fn set(display: u32, want: Mode) -> Result<(), String> {
        let dev = device(display).ok_or("no such display")?;
        let mut d = dm();
        d.dmPelsWidth = want.w;
        d.dmPelsHeight = want.h;
        d.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if want.hz > 0 {
            d.dmDisplayFrequency = want.hz;
            d.dmFields |= DM_DISPLAYFREQUENCY;
        }
        // flags 0: for now, not saved in the registry (a restart brings the old one back)
        let r = unsafe { ChangeDisplaySettingsExW(dev.as_ptr(), &d, std::ptr::null_mut(), 0, std::ptr::null()) };
        if r == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(format!("ChangeDisplaySettingsEx: {r}"))
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::Mode;

    pub fn current(_display: u32) -> Option<Mode> {
        None
    }

    pub fn modes(_display: u32) -> Vec<Mode> {
        Vec::new()
    }

    pub fn set(_display: u32, _want: Mode) -> Result<(), String> {
        Err("unsupported".into())
    }
}

pub use imp::{current, set};

/// The sizes a display offers, biggest first, each once (at the current refresh where it
/// has that, else its highest), at least 800 wide.
pub fn modes(display: u32) -> Vec<Mode> {
    let cur_hz = current(display).map(|m| m.hz).unwrap_or(0);
    let mut out: Vec<Mode> = Vec::new();
    for m in imp::modes(display).into_iter().filter(|m| m.w >= 800 && m.h >= 600) {
        match out.iter_mut().find(|o| o.w == m.w && o.h == m.h) {
            Some(o) => {
                if (m.hz == cur_hz) || (o.hz != cur_hz && m.hz > o.hz) {
                    o.hz = m.hz;
                }
            }
            None => out.push(m),
        }
    }
    out.sort_by(|a, b| (b.w * b.h, b.w).cmp(&(a.w * a.h, a.w)));
    out
}

#[cfg(test)]
mod tests {
    /// MANYDESK_TESTMODES=1: lists the first display's modes and sets the current one again
    /// (nothing visible changes).
    #[test]
    fn modes_of_main_display() {
        if std::env::var_os("MANYDESK_TESTMODES").is_none() {
            return;
        }
        let s = crate::media::capture::screens(false);
        let id = s.first().expect("a display").id;
        let cur = super::current(id).expect("current mode");
        let list = super::modes(id);
        println!("current {cur:?}, {} modes: {:?}", list.len(), &list[..list.len().min(6)]);
        assert!(list.iter().any(|m| m.w == cur.w && m.h == cur.h));
        super::set(id, cur).expect("set the current mode");
    }
}
