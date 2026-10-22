// Windows: the ManyDesk service, so a computer can be reached with nobody signed in.
//
// The service runs as LocalSystem and starts with Windows. Being in session 0 it draws
// nothing itself; it launches a copy of ManyDesk (--background) into the interactive
// console session and keeps it running there — after a sign-out, a restart, or while the
// sign-in screen is up. It also sends Ctrl+Alt+Del when a viewer asks (a service is the
// only thing allowed to), through a shared event the running app waits on.
//
//   manydesk --install-service     register and start it (needs admin)
//   manydesk --uninstall-service   stop and remove it
//   manydesk --service             the service entry point (called by Windows)
#![cfg(windows)]
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::RemoteDesktop::*;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::*;

const SERVICE_NAME: &str = "ManyDesk";
const SAS_EVENT: &str = r"Global\ManyDeskSAS";

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

static STOP: AtomicBool = AtomicBool::new(false);
static mut STATUS_HANDLE: SERVICE_STATUS_HANDLE = std::ptr::null_mut();

/// Viewer asked for Ctrl+Alt+Del: signal the service's SAS worker (or, if we are the
/// session app, the service reads this event and calls SendSAS).
pub fn request_sas() -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent};
    unsafe {
        let h = OpenEventW(0x0002 /* EVENT_MODIFY_STATE */, 0, wide(SAS_EVENT).as_ptr());
        if h.is_null() {
            return Err("service not running".into());
        }
        let ok = SetEvent(h) != 0;
        CloseHandle(h);
        if ok {
            Ok(())
        } else {
            Err("SetEvent".into())
        }
    }
}

pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cmd = format!("\"{}\" --service", exe.display());
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            return Err("need administrator".into());
        }
        let svc = CreateServiceW(
            scm,
            wide(SERVICE_NAME).as_ptr(),
            wide("ManyDesk Remote Access").as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            wide(&cmd).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(), // LocalSystem
            std::ptr::null(),
        );
        let err = GetLastError();
        if !svc.is_null() {
            StartServiceW(svc, 0, std::ptr::null());
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        if svc.is_null() && err != 1073 {
            // 1073 = ERROR_SERVICE_EXISTS
            return Err(format!("CreateService failed ({err})"));
        }
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            return Err("need administrator".into());
        }
        let svc = OpenServiceW(scm, wide(SERVICE_NAME).as_ptr(), SERVICE_ALL_ACCESS);
        if !svc.is_null() {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
            DeleteService(svc);
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
    }
    Ok(())
}

pub fn installed() -> bool {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let svc = OpenServiceW(scm, wide(SERVICE_NAME).as_ptr(), SERVICE_QUERY_STATUS);
        let yes = !svc.is_null();
        if yes {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        yes
    }
}

/// Called when Windows starts the service (main.rs sees --service).
pub fn run() {
    unsafe {
        let table = [
            SERVICE_TABLE_ENTRYW { lpServiceName: wide(SERVICE_NAME).as_ptr() as *mut u16, lpServiceProc: Some(service_main) },
            SERVICE_TABLE_ENTRYW { lpServiceName: std::ptr::null_mut(), lpServiceProc: None },
        ];
        StartServiceCtrlDispatcherW(table.as_ptr());
    }
}

unsafe extern "system" fn handler(control: u32) {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::SeqCst);
        set_state(SERVICE_STOP_PENDING);
    }
}

unsafe fn set_state(state: u32) {
    let mut st: SERVICE_STATUS = std::mem::zeroed();
    st.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
    st.dwCurrentState = state;
    st.dwControlsAccepted = if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN } else { 0 };
    SetServiceStatus(STATUS_HANDLE, &mut st);
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    STATUS_HANDLE = RegisterServiceCtrlHandlerW(wide(SERVICE_NAME).as_ptr(), Some(handler));
    if STATUS_HANDLE.is_null() {
        return;
    }
    set_state(SERVICE_RUNNING);

    // The SAS event viewers trigger through request_sas().
    let sas = CreateEventW(std::ptr::null(), 0, 0, wide(SAS_EVENT).as_ptr());

    let mut child: Option<HANDLE> = None;
    let mut last_session = 0xFFFF_FFFFu32;
    while !STOP.load(Ordering::SeqCst) {
        // Ctrl+Alt+Del requests
        if !sas.is_null() && WaitForSingleObject(sas, 0) == WAIT_OBJECT_0 {
            send_sas();
        }
        // Keep an app running in the active console session.
        let session = WTSGetActiveConsoleSessionId();
        let alive = child.map(|h| WaitForSingleObject(h, 0) != WAIT_OBJECT_0).unwrap_or(false);
        if session != 0xFFFF_FFFF && (!alive || session != last_session) {
            if let Some(h) = child.take() {
                CloseHandle(h);
            }
            if let Some(h) = launch_in_session(session) {
                child = Some(h);
                last_session = session;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if let Some(h) = child.take() {
        CloseHandle(h);
    }
    set_state(SERVICE_STOPPED);
}

/// SendSAS(FALSE) presses Ctrl+Alt+Del. It lives in sas.dll.
unsafe fn send_sas() {
    type SendSas = unsafe extern "system" fn(i32);
    let dll = LoadLibraryW(wide("sas.dll").as_ptr());
    if dll.is_null() {
        return;
    }
    let p = GetProcAddress(dll, b"SendSAS\0".as_ptr());
    if let Some(f) = p {
        let f: SendSas = std::mem::transmute(f);
        f(0);
    }
}

/// Starts manydesk --background in `session` as the signed-in user (or SYSTEM on the
/// sign-in screen) so it can capture and control that desktop.
unsafe fn launch_in_session(session: u32) -> Option<HANDLE> {
    let mut token: HANDLE = std::ptr::null_mut();
    if WTSQueryUserToken(session, &mut token) == 0 {
        // No user yet (sign-in screen): run as SYSTEM on the Winlogon desktop.
        return launch_system(session);
    }
    let exe = std::env::current_exe().ok()?;
    let mut cmd = wide(&format!("\"{}\" --background", exe.display()));
    let mut env: *mut std::ffi::c_void = std::ptr::null_mut();
    CreateEnvironmentBlock(&mut env, token, 0);
    let mut si: STARTUPINFOW = std::mem::zeroed();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut desktop = wide(r"winsta0\default");
    si.lpDesktop = desktop.as_mut_ptr();
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let ok = CreateProcessAsUserW(
        token,
        std::ptr::null(),
        cmd.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        0,
        CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
        env,
        std::ptr::null(),
        &si,
        &mut pi,
    );
    if !env.is_null() {
        DestroyEnvironmentBlock(env);
    }
    CloseHandle(token);
    if ok != 0 {
        CloseHandle(pi.hThread);
        Some(pi.hProcess)
    } else {
        None
    }
}

/// Sign-in screen: a SYSTEM process on the Winlogon desktop (best effort — enough for
/// input; capturing the secure desktop depends on the Windows build's policy).
unsafe fn launch_system(session: u32) -> Option<HANDLE> {
    let proc = GetCurrentProcess();
    let mut tok: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(proc, TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY, &mut tok) == 0 {
        return None;
    }
    let mut dup: HANDLE = std::ptr::null_mut();
    let ok_dup = DuplicateTokenEx(tok, TOKEN_ALL_ACCESS, std::ptr::null(), SecurityIdentification, TokenPrimary, &mut dup);
    CloseHandle(tok);
    if ok_dup == 0 {
        return None;
    }
    SetTokenInformation(dup, TokenSessionId, &session as *const _ as *const _, std::mem::size_of::<u32>() as u32);
    let exe = std::env::current_exe().ok()?;
    let mut cmd = wide(&format!("\"{}\" --background", exe.display()));
    let mut si: STARTUPINFOW = std::mem::zeroed();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut desktop = wide(r"winsta0\winlogon");
    si.lpDesktop = desktop.as_mut_ptr();
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let ok = CreateProcessAsUserW(dup, std::ptr::null(), cmd.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW, std::ptr::null_mut(), std::ptr::null(), &si, &mut pi);
    CloseHandle(dup);
    if ok != 0 {
        CloseHandle(pi.hThread);
        Some(pi.hProcess)
    } else {
        None
    }
}
