// Native media shared by every platform: screen capture and H.264 encoding.
pub mod capture;
pub mod encode;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(windows)]
mod win;

#[tauri::command]
pub async fn screens_list() -> Vec<capture::Screen> {
    tauri::async_runtime::spawn_blocking(capture::screens).await.unwrap_or_default()
}
