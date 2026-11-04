// A virtual display on the host (RustDesk's "virtual display"; for a Mac with no monitor,
// or one more screen to work on). macOS only, through CoreGraphics' CGVirtualDisplay
// classes (what DeskPad and BetterDisplay use; no driver, macOS 11+). The display lives as
// long as the VDisplay value. Windows would need a signed display driver (RustDesk ships
// one): not offered there.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::{NSArray, NSSize, NSString};

    pub struct VDisplay {
        _display: Retained<AnyObject>,
        pub id: u32,
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, String> {
        AnyClass::get(name).ok_or_else(|| "virtual displays are not available on this macOS".to_string())
    }

    /// A display of `w`×`h` pixels at 60 Hz.
    pub fn create(w: u32, h: u32) -> Result<VDisplay, String> {
        unsafe {
            let desc: Option<Retained<AnyObject>> = msg_send![class(c"CGVirtualDisplayDescriptor")?, new];
            let desc = desc.ok_or("CGVirtualDisplayDescriptor")?;
            let name = NSString::from_str("ManyDesk");
            let _: () = msg_send![&*desc, setName: &*name];
            let _: () = msg_send![&*desc, setMaxPixelsWide: w];
            let _: () = msg_send![&*desc, setMaxPixelsHigh: h];
            // ~110 dpi, a usual desktop monitor
            let _: () = msg_send![&*desc, setSizeInMillimeters: NSSize::new(w as f64 * 0.23, h as f64 * 0.23)];
            let _: () = msg_send![&*desc, setVendorID: 0x4d44u32];
            let _: () = msg_send![&*desc, setProductID: 0x5644u32];
            let _: () = msg_send![&*desc, setSerialNum: 1u32];
            let queue = dispatch2::DispatchQueue::main();
            let _: () = msg_send![&*desc, setQueue: &*(queue as *const _ as *const AnyObject)];

            let alloc: *mut AnyObject = msg_send![class(c"CGVirtualDisplay")?, alloc];
            let display: *mut AnyObject = msg_send![alloc, initWithDescriptor: &*desc];
            let display = Retained::from_raw(display).ok_or("CGVirtualDisplay")?;

            let alloc: *mut AnyObject = msg_send![class(c"CGVirtualDisplayMode")?, alloc];
            let mode: *mut AnyObject = msg_send![alloc, initWithWidth: w as usize, height: h as usize, refreshRate: 60.0f64];
            let mode = Retained::from_raw(mode).ok_or("CGVirtualDisplayMode")?;
            let settings: Option<Retained<AnyObject>> = msg_send![class(c"CGVirtualDisplaySettings")?, new];
            let settings = settings.ok_or("CGVirtualDisplaySettings")?;
            let _: () = msg_send![&*settings, setHiDPI: 0u32];
            let modes = NSArray::from_retained_slice(&[mode]);
            let _: () = msg_send![&*settings, setModes: &*modes];
            let ok: bool = msg_send![&*display, applySettings: &*settings];
            if !ok {
                return Err("applySettings".into());
            }
            let id: u32 = msg_send![&*display, displayID];
            Ok(VDisplay { _display: display, id })
        }
    }

    pub const SUPPORTED: bool = true;
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub struct VDisplay {
        pub id: u32,
    }

    pub fn create(_w: u32, _h: u32) -> Result<VDisplay, String> {
        Err("unsupported".into())
    }

    pub const SUPPORTED: bool = false;
}

pub use imp::{create, VDisplay, SUPPORTED};

#[cfg(test)]
mod tests {
    /// MANYDESK_TESTVD=1: adds a 1280×800 virtual display for two seconds and checks the
    /// display list has it.
    #[test]
    fn virtual_display_appears() {
        if std::env::var_os("MANYDESK_TESTVD").is_none() || !super::SUPPORTED {
            return;
        }
        let vd = super::create(1280, 800).expect("create");
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let list = crate::media::capture::screens(false);
        println!("vd {} in {:?}", vd.id, list.iter().map(|s| (s.id, s.width, s.height)).collect::<Vec<_>>());
        assert!(list.iter().any(|s| s.id == vd.id && s.width == 1280));
        drop(vd);
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(!crate::media::capture::screens(false).iter().any(|s| s.width == 1280 && s.height == 800));
    }
}
