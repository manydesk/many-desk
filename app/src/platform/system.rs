// Host actions on the operating system a viewer can ask for: restart the computer, lock
// the screen, Ctrl+Alt+Del (Windows, through the ManyDesk service), and starting with the
// system. Each returns a short error text for the viewer when it can't be done.
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let mut c = Command::new(cmd);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match c.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let e: String = String::from_utf8_lossy(&o.stderr).trim().chars().take(200).collect();
            Err(if e.is_empty() { format!("{cmd}: {}", o.status) } else { e })
        }
        Err(e) => Err(format!("{cmd}: {e}")),
    }
}

/// Restarts the computer (apps get the usual chance to quit).
pub fn reboot() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // loginwindow's restart event: no confirmation dialog, apps are asked to quit.
        return run("osascript", &["-e", "tell application \"loginwindow\" to «event aevtrrst»"]).or_else(|_| run("osascript", &["-e", "tell application \"System Events\" to restart"]));
    }
    #[cfg(windows)]
    {
        return run("shutdown", &["/r", "/t", "3", "/c", "ManyDesk"]);
    }
    #[cfg(target_os = "linux")]
    {
        return run("systemctl", &["reboot"]);
    }
    #[allow(unreachable_code)]
    Err("unsupported".into())
}

/// Locks the screen (the session stays, the user has to sign in again).
pub fn lock_screen() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // Ctrl+Cmd+Q, the system's own lock shortcut, from a private event source.
        return mac::lock();
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Shutdown::LockWorkStation;
        return if unsafe { LockWorkStation() } != 0 { Ok(()) } else { Err("LockWorkStation failed".into()) };
    }
    #[cfg(target_os = "linux")]
    {
        return run("loginctl", &["lock-session"]).or_else(|_| run("xdg-screensaver", &["lock"]));
    }
    #[allow(unreachable_code)]
    Err("unsupported".into())
}

/// Ctrl+Alt+Del. Windows only lets a service send it: the ManyDesk service waits on a
/// named event for this (see service.rs).
pub fn secure_attention() -> Result<(), String> {
    #[cfg(windows)]
    {
        return crate::platform::service::request_sas();
    }
    #[allow(unreachable_code)]
    Err("unsupported".into())
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state: i32) -> *mut std::ffi::c_void;
        fn CGEventCreateKeyboardEvent(source: *mut std::ffi::c_void, key: u16, down: bool) -> *mut std::ffi::c_void;
        fn CGEventSetFlags(event: *mut std::ffi::c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut std::ffi::c_void);
        fn CFRelease(cf: *const std::ffi::c_void);
    }
    pub fn lock() -> Result<(), String> {
        const CMD: u64 = 1 << 20;
        const CTRL: u64 = 1 << 18;
        unsafe {
            let src = CGEventSourceCreate(1); // combined session state
            for down in [true, false] {
                let e = CGEventCreateKeyboardEvent(src, 12, down); // Q
                if e.is_null() {
                    return Err("CGEvent".into());
                }
                CGEventSetFlags(e, CMD | CTRL);
                CGEventPost(0, e); // kCGHIDEventTap
                CFRelease(e);
            }
            if !src.is_null() {
                CFRelease(src);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- autostart ---

/// The command that starts ManyDesk in the background at sign-in.
fn exe() -> Option<String> {
    std::env::current_exe().ok().map(|p| p.to_string_lossy().to_string())
}

#[cfg(target_os = "macos")]
fn agent_path() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Library/LaunchAgents/az.manydesk.native.plist"))
}

/// Whether ManyDesk starts by itself when this user signs in.
pub fn autostart_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        return agent_path().is_some_and(|p| p.exists());
    }
    #[cfg(windows)]
    {
        return run("reg", &["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "ManyDesk"]).is_ok();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_autostart().is_some_and(|p| p.exists());
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(target_os = "linux")]
fn linux_autostart() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|b| b.config_dir().join("autostart/manydesk.desktop"))
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let exe = exe().ok_or("no exe")?;
    #[cfg(target_os = "macos")]
    {
        let path = agent_path().ok_or("no home")?;
        if !on {
            // The file alone: launchd forgets the agent at the next sign-in. (Unloading it
            // now would end this very process when launchd started it.)
            return std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| e.to_string());
        }
        let plist = agent_plist(&exe);
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        return std::fs::write(&path, plist).map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        return if on { run("reg", &["add", key, "/v", "ManyDesk", "/t", "REG_SZ", "/d", &format!("\"{exe}\" --background"), "/f"]) } else { run("reg", &["delete", key, "/v", "ManyDesk", "/f"]).or(Ok(())) };
    }
    #[cfg(target_os = "linux")]
    {
        let path = linux_autostart().ok_or("no config dir")?;
        if !on {
            return std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|e| e.to_string());
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let entry = format!("[Desktop Entry]\nType=Application\nName=ManyDesk\nExec=\"{exe}\" --background\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n");
        return std::fs::write(&path, entry).map_err(|e| e.to_string());
    }
    #[allow(unreachable_code)]
    {
        let _ = (on, exe);
        Err("unsupported".into())
    }
}

/// The launch agent: launchd starts ManyDesk in the background at sign-in and again if it
/// crashes (KeepAlive, unless it quit cleanly — the user's Quit, or an update that starts
/// the new version itself). The program is the bundle's own executable, so macOS shows it
/// as ManyDesk under Login Items (AssociatedBundleIdentifiers) and the permissions granted
/// to the app apply.
#[cfg(target_os = "macos")]
fn agent_plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>az.manydesk.native</string>
  <key>ProgramArguments</key><array><string>{}</string><string>--background</string></array>
  <key>AssociatedBundleIdentifiers</key><array><string>az.manydesk.native</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
"#,
        xml(exe)
    )
}

/// An agent written by an older version (or for the app at another place) is brought up
/// to date; it takes effect at the next sign-in.
#[cfg(target_os = "macos")]
pub fn refresh_autostart() {
    let (Some(path), Some(exe)) = (agent_path(), exe()) else { return };
    // only the agent of this very app (not one that starts a copy somewhere else)
    let Some(bundle) = exe.find(".app/Contents/MacOS/").map(|i| &exe[..i + 4]) else { return };
    if let Ok(old) = std::fs::read_to_string(&path) {
        let new = agent_plist(&exe);
        if old != new && old.contains(&xml(bundle)) {
            let _ = std::fs::write(&path, new);
        }
    }
}

#[cfg(target_os = "macos")]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
