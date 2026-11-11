// Windows: one ManyDesk per signed-in session and settings folder. Starting it again (the
// Start menu while the service keeps a copy running, autostart and a double click) brings
// the running copy's window forward instead of a second copy with the same address, as
// AnyDesk does. Test copies with their own MANYDESK_CONFIG_DIR are copies of their own.
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, BOOL, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, GetCurrentProcessId, OpenEventW, SetEvent, WaitForSingleObject, INFINITE};
use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SetForegroundWindow, ShowWindow, ASFW_ANY, SW_RESTORE};

/// Set when another start asked this copy to show itself (the UI takes it).
pub static SHOW: AtomicBool = AtomicBool::new(false);

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Names in this session's namespace ("Local\"), per settings folder.
fn names() -> (String, String) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::store::config_dir().to_string_lossy().to_lowercase().hash(&mut h);
    let id = format!("{:016x}", h.finish());
    (format!("Local\\ManyDesk-{id}"), format!("Local\\ManyDeskShow-{id}"))
}

/// Held for as long as this copy is the one: dropping it lets another start.
pub struct Guard {
    mutex: HANDLE,
    event: HANDLE,
}

unsafe impl Send for Guard {}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.mutex);
            CloseHandle(self.event);
        }
    }
}

/// Some(guard): this is the only copy. None: another copy runs in this session; it has
/// been asked to show its window, and this one should quit.
pub fn claim() -> Option<Guard> {
    let (mutex_name, event_name) = names();
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, wide(&mutex_name).as_ptr());
        if mutex.is_null() {
            return Some(Guard { mutex, event: std::ptr::null_mut() }); // no guard possible: run anyway
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            // this start came from the user, so it may bring a window forward: hand that on
            AllowSetForegroundWindow(ASFW_ANY);
            let ev = OpenEventW(0x0002 /* EVENT_MODIFY_STATE */, 0, wide(&event_name).as_ptr());
            if !ev.is_null() {
                SetEvent(ev);
                CloseHandle(ev);
            }
            return None;
        }
        let event = CreateEventW(std::ptr::null(), 0, 0, wide(&event_name).as_ptr());
        if !event.is_null() {
            let ev = event as usize;
            std::thread::Builder::new()
                .name("show-requests".into())
                .spawn(move || loop {
                    if WaitForSingleObject(ev as HANDLE, INFINITE) != WAIT_OBJECT_0 {
                        return;
                    }
                    SHOW.store(true, Ordering::SeqCst);
                    crate::bus::wake();
                    // straight through Windows: a minimized window draws no frames, so the
                    // UI would not see SHOW until it is shown
                    restore_main_window();
                })
                .ok();
        }
        Some(Guard { mutex, event })
    }
}

/// Restores and focuses this process's main window (the one titled "ManyDesk"; not the
/// host overlay or a session's own window).
fn restore_main_window() {
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != GetCurrentProcessId() {
            return 1;
        }
        let mut buf = [0u16; 64];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "ManyDesk" {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
            return 0;
        }
        1
    }
    unsafe {
        EnumWindows(Some(each), 0);
    }
}
