// Android glue. The app runs in a NativeActivity (android/app/.../MainActivity.java); what
// Rust can't do itself goes through the static methods of az.manydesk.Bridge:
//   - screen sharing: MediaProjection → ImageReader frames arrive in `nativeFrame`
//     (ScreenCapture.java) and feed `ScreenSource`, the host's capture source here;
//   - remote control of this phone: InputService.java, an AccessibilityService, plays the
//     viewer's taps, drags, scrolls, keys and text as gestures and actions (`inject`);
//   - the clipboard (ClipboardManager) and the soft keyboard.
// stderr (all the eprintln! diagnostics) is copied to logcat under the tag "ManyDesk".
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use jni::objects::{GlobalRef, JByteBuffer, JClass, JObject, JString, JValue};
use jni::sys::{jint, jfloat};
use jni::{JNIEnv, JavaVM};
use winit::platform::android::activity::AndroidApp;

use crate::media::capture::{Frame, Screen};

pub fn init(app: &AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("ManyDesk"));
    // Settings, the recent list and received files live in the app's own storage.
    if let Some(dir) = app.internal_data_path() {
        std::env::set_var("HOME", &dir);
        let tmp = dir.join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        std::env::set_var("TMPDIR", &tmp);
    }
    stderr_to_logcat();
    // Tests: `adb shell setprop debug.manydesk.env "MANYDESK_PERF=1;MANYDESK_X=y"` sets
    // the same environment the desktop test hooks read.
    if let Some(env) = prop(c"debug.manydesk.env") {
        for kv in env.split(';') {
            if let Some((k, v)) = kv.split_once('=') {
                std::env::set_var(k.trim(), v.trim());
            }
        }
    }
}

/// A system property ("" or unset: None).
fn prop(key: &std::ffi::CStr) -> Option<String> {
    let mut buf = [0u8; 92]; // PROP_VALUE_MAX
    let n = unsafe { libc::__system_property_get(key.as_ptr(), buf.as_mut_ptr().cast()) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).trim().to_string()).filter(|s| !s.is_empty())
}

/// Copies fd 2 into the log, line by line.
fn stderr_to_logcat() {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    unsafe { libc::dup2(fds[1], 2) };
    let read = fds[0];
    std::thread::spawn(move || {
        use std::io::BufRead;
        use std::os::fd::FromRawFd;
        let f = unsafe { std::fs::File::from_raw_fd(read) };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            log::info!("{line}");
        }
    });
}

// ------------------------------------------------------------------- the bridge ---

fn vm() -> Option<JavaVM> {
    let ctx = ndk_context::android_context();
    unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()
}

/// az.manydesk.Bridge, loaded through the activity's class loader (FindClass on a native
/// thread only sees the system classes).
fn bridge(env: &mut JNIEnv) -> Option<&'static GlobalRef> {
    static BRIDGE: OnceLock<Option<GlobalRef>> = OnceLock::new();
    BRIDGE
        .get_or_init(|| {
            let activity = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };
            let loader = env.call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).ok()?.l().ok()?;
            let name = env.new_string("az.manydesk.Bridge").ok()?;
            let class = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)]).ok()?.l().ok()?;
            env.new_global_ref(class).ok()
        })
        .as_ref()
}

/// Runs `f` with an attached JNIEnv and the Bridge class; Java exceptions are cleared.
fn with_bridge<T>(f: impl FnOnce(&mut JNIEnv, &JClass) -> Option<T>) -> Option<T> {
    let vm = vm()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    let class = bridge(&mut env)?;
    let class: &JClass = class.as_obj().into();
    let out = f(&mut env, class);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    out
}

pub fn set_clipboard_text(text: &str) {
    with_bridge(|env, c| {
        let s = env.new_string(text).ok()?;
        env.call_static_method(c, "setClip", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).ok().map(|_| ())
    });
}

pub fn clipboard_text() -> Option<String> {
    with_bridge(|env, c| {
        let o = env.call_static_method(c, "getClip", "()Ljava/lang/String;", &[]).ok()?.l().ok()?;
        if o.is_null() {
            return None;
        }
        env.get_string(&JString::from(o)).ok().map(Into::into)
    })
}

/// One remote-control event (input.rs's JSON, coordinates in this display's pixels).
pub fn inject(cmd: &str) {
    with_bridge(|env, c| {
        let s = env.new_string(cmd).ok()?;
        env.call_static_method(c, "inject", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).ok().map(|_| ())
    });
}

/// The session camera (CameraCapture.java) on or off; pictures come to media/camera.rs.
pub fn camera(on: bool) {
    with_bridge(|env, c| env.call_static_method(c, if on { "startCamera" } else { "stopCamera" }, "()V", &[]).ok().map(|_| ()));
}

/// Whether the accessibility service (remote control of this phone) is on.
pub fn input_ready() -> bool {
    with_bridge(|env, c| env.call_static_method(c, "inputReady", "()Z", &[]).ok()?.z().ok()).unwrap_or(false)
}

/// The texts Android shows for ManyDesk (notifications), in the app's language.
fn push_texts() {
    let kv: Vec<String> = ["n.reqChannel", "n.reqText", "n.shareChannel", "n.shareText"].iter().flat_map(|k| [k.to_string(), crate::i18n::t(k)]).collect();
    with_bridge(move |env, c| {
        let arr = env.new_object_array(kv.len() as i32, "java/lang/String", JObject::null()).ok()?;
        for (i, s) in kv.iter().enumerate() {
            let js = env.new_string(s).ok()?;
            env.set_object_array_element(&arr, i as i32, js).ok()?;
        }
        env.call_static_method(c, "setTexts", "([Ljava/lang/String;)V", &[JValue::Object(&arr)]).ok().map(|_| ())
    });
}

/// Someone asks to connect: while ManyDesk is out of sight a notification says so (a tap
/// opens the app with the request); gone once the request is answered. `None`: nothing waits.
pub fn notify_request(name: Option<&str>) {
    static SHOWN: Mutex<Option<String>> = Mutex::new(None);
    let mut shown = SHOWN.lock().unwrap();
    if shown.as_deref() == name {
        return;
    }
    *shown = name.map(str::to_string);
    push_texts();
    let name = name.map(str::to_string);
    with_bridge(move |env, c| {
        let s = match &name {
            Some(n) => env.new_string(n).ok()?,
            None => JString::from(JObject::null()),
        };
        env.call_static_method(c, "notifyRequest", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).ok().map(|_| ())
    });
}

/// Settings → Accessibility, where remote control of this phone is turned on.
pub fn open_input_settings() {
    with_bridge(|env, c| env.call_static_method(c, "openInputSettings", "()V", &[]).ok().map(|_| ()));
}

/// ManyDesk's App info page (where "Allow restricted settings" is).
pub fn open_app_info() {
    with_bridge(|env, c| env.call_static_method(c, "openAppInfo", "()V", &[]).ok().map(|_| ()));
}

/// The shared storage (/storage/emulated/0) when ManyDesk may read it ("All files access").
pub fn shared_storage() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from("/storage/emulated/0");
    std::fs::read_dir(&p).ok().map(|_| p)
}

/// Whether "All files access" is on (the file manager needs it for the phone's files).
pub fn files_access() -> bool {
    with_bridge(|env, c| env.call_static_method(c, "filesAccess", "()Z", &[]).ok()?.z().ok()).unwrap_or(false)
}

/// Opens the system page that grants "All files access".
pub fn open_files_access() {
    with_bridge(|env, c| env.call_static_method(c, "openFilesAccess", "()V", &[]).ok().map(|_| ()));
}

/// Shows or hides the soft keyboard (typing into a remote session).
pub fn soft_keyboard(show: bool) {
    with_bridge(|env, c| env.call_static_method(c, "keyboard", "(Z)V", &[JValue::Bool(show as u8)]).ok().map(|_| ()));
}

/// Keeps the soft keyboard up exactly while egui wants typing (a focused text field, or a
/// remote session taking the keyboard).
pub fn sync_keyboard(want: bool) {
    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SHOWN.swap(want, std::sync::atomic::Ordering::Relaxed) != want {
        soft_keyboard(want);
    }
}

/// "Pixel 7" (the maker's model name, from the system properties).
pub fn device_name() -> Option<String> {
    let model = prop(c"ro.product.model")?;
    let maker = prop(c"ro.product.manufacturer").unwrap_or_default();
    Some(if maker.is_empty() || model.to_lowercase().starts_with(&maker.to_lowercase()) { model } else { format!("{maker} {model}") })
}

// ----------------------------------------------------------- remote control ---

/// Host sessions whose viewer may control this phone (kept by the core).
static CONTROL: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn set_control(sids: Vec<u32>) {
    *CONTROL.lock().unwrap() = sids;
}

/// A host session's message, on the network thread: the viewer's taps, keys and text go
/// to the accessibility service from here. The core sees them on the UI thread, which
/// Android stops while ManyDesk is in the background — and controlling the phone is what
/// puts it there (the first tap that opens another app). True: handled.
pub fn early_input(sid: u32, ev: &serde_json::Value) -> bool {
    static INPUT: OnceLock<crate::input::Input> = OnceLock::new();
    if ev["e"] != "ctl" || !CONTROL.lock().unwrap().contains(&sid) {
        return false;
    }
    let Ok(m) = serde_json::from_str::<serde_json::Value>(ev["text"].as_str().unwrap_or("")) else { return false };
    if !matches!(m["t"].as_str(), Some("move" | "down" | "up" | "scroll" | "key" | "text" | "release")) {
        return false;
    }
    let Some(screen) = screens().into_iter().next() else { return false };
    INPUT.get_or_init(crate::input::Input::new).handle(&screen, &m);
    true
}

// --------------------------------------------------------------- the display ---

/// This phone's screen: (width, height) in pixels and density, set by MainActivity.
static DISPLAY: Mutex<(u32, u32, f32)> = Mutex::new((1080, 2400, 2.75));

#[no_mangle]
pub extern "system" fn Java_az_manydesk_Bridge_nativeDisplay(_env: JNIEnv, _c: JClass, w: jint, h: jint, density: jfloat) {
    *DISPLAY.lock().unwrap() = (w.max(1) as u32, h.max(1) as u32, density);
}

/// The system bars over the window: (top, bottom, left, right) in pixels.
static INSETS: Mutex<(i32, i32, i32, i32)> = Mutex::new((0, 0, 0, 0));

#[no_mangle]
pub extern "system" fn Java_az_manydesk_Bridge_nativeInsets(_env: JNIEnv, _c: JClass, top: jint, bottom: jint, left: jint, right: jint) {
    *INSETS.lock().unwrap() = (top, bottom, left, right);
}

/// What the status and navigation bars cover, in points: (top, bottom).
pub fn insets() -> (f32, f32) {
    let (t, b, _, _) = *INSETS.lock().unwrap();
    let density = DISPLAY.lock().unwrap().2.max(0.5);
    (t.max(0) as f32 / density, b.max(0) as f32 / density)
}

pub fn screens() -> Vec<Screen> {
    // While sharing, the frames say the size (rotation included).
    let (w, h) = CAPTURE.latest.lock().unwrap().1.as_ref().map(|f| (f.width, f.height)).unwrap_or_else(|| {
        let d = *DISPLAY.lock().unwrap();
        (d.0, d.1)
    });
    vec![Screen { id: 0, name: "Phone".into(), x: 0, y: 0, width: w, height: h, scale: 1.0, thumb: String::new() }]
}

// ----------------------------------------------------------- screen sharing ---

struct Shot {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct Capture {
    latest: Mutex<(u64, Option<Arc<Shot>>)>,
    cond: Condvar,
    users: Mutex<usize>,
}

static CAPTURE: Capture = Capture { latest: Mutex::new((0, None)), cond: Condvar::new(), users: Mutex::new(0) };

/// A frame from ScreenCapture.java's ImageReader (RGBA_8888, rows `stride` bytes apart).
#[no_mangle]
pub extern "system" fn Java_az_manydesk_ScreenCapture_nativeFrame(env: JNIEnv, _c: JClass, buf: JByteBuffer, w: jint, h: jint, stride: jint) {
    let (Ok(ptr), Ok(cap)) = (env.get_direct_buffer_address(&buf), env.get_direct_buffer_capacity(&buf)) else { return };
    let (w, h, stride) = (w.max(0) as usize, h.max(0) as usize, stride.max(0) as usize);
    if w == 0 || h == 0 || stride < w * 4 || cap < stride * (h - 1) + w * 4 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(ptr, cap) };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        rgba.extend_from_slice(&data[row * stride..row * stride + w * 4]);
    }
    let mut l = CAPTURE.latest.lock().unwrap();
    l.0 += 1;
    l.1 = Some(Arc::new(Shot { width: w as u32, height: h as u32, rgba }));
    CAPTURE.cond.notify_all();
}

/// The user turned sharing down (or Android stopped it).
#[no_mangle]
pub extern "system" fn Java_az_manydesk_ScreenCapture_nativeStopped(_env: JNIEnv, _c: JClass) {
    eprintln!("[android] screen capture stopped");
}

/// The host's capture source: asks for MediaProjection when the first session starts and
/// stops it after the last one.
pub struct ScreenSource {
    seen: u64,
    last: Option<Arc<Shot>>,
}

impl ScreenSource {
    pub fn open(_screen: Option<&Screen>) -> Result<Self, String> {
        let mut users = CAPTURE.users.lock().unwrap();
        if *users == 0 {
            push_texts();
            with_bridge(|env, c| env.call_static_method(c, "startCapture", "()V", &[]).ok().map(|_| ()));
        }
        *users += 1;
        Ok(ScreenSource { seen: 0, last: None })
    }

    pub fn grab(&mut self, wait: Duration) -> Option<Frame<'_>> {
        let l = CAPTURE.latest.lock().unwrap();
        let (l, _) = CAPTURE.cond.wait_timeout_while(l, wait, |l| l.0 == self.seen).unwrap();
        if l.0 != self.seen {
            self.seen = l.0;
            self.last = l.1.clone();
        }
        drop(l);
        let f = self.last.as_ref()?;
        Some(Frame { data: &f.rgba, width: f.width as usize, height: f.height as usize, stride: f.width as usize * 4, bgra: false })
    }
}

impl Drop for ScreenSource {
    fn drop(&mut self) {
        let mut users = CAPTURE.users.lock().unwrap();
        *users = users.saturating_sub(1);
        if *users == 0 {
            with_bridge(|env, c| env.call_static_method(c, "stopCapture", "()V", &[]).ok().map(|_| ()));
            *CAPTURE.latest.lock().unwrap() = (0, None);
        }
    }
}
