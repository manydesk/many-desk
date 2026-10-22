// WebView capability probe for ManyDesk on Tauri: loads dist/index.html, which runs the
// feature checks and calls `report` with the JSON result; we print it and quit.
use tauri::Manager;

#[tauri::command]
fn report(app: tauri::AppHandle, json: String) {
    println!("PROBE_RESULT {json}");
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let _win = app.get_webview_window("main").unwrap();
            // WebKitGTK ships WebRTC and media capture switched off; turn them on.
            #[cfg(target_os = "linux")]
            _win.with_webview(|wv| {
                use webkit2gtk::{SettingsExt, WebViewExt};
                let view = wv.inner();
                if let Some(s) = view.settings() {
                    s.set_enable_webrtc(true);
                    s.set_enable_media_stream(true);
                    s.set_enable_mediasource(true);
                    s.set_media_playback_requires_user_gesture(false);
                    println!("LINUX_SETTINGS webrtc={} media_stream={}", s.enables_webrtc(), s.enables_media_stream());
                }
                // Grant camera/mic/display permission requests (the app decides when to ask).
                use webkit2gtk::{PermissionRequestExt};
                view.connect_permission_request(|_, req| { req.allow(); true });
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![report])
        .run(tauri::generate_context!())
        .expect("tauri failed");
}
