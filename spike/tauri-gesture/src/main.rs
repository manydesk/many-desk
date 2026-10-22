// WebView capability probe for ManyDesk on Tauri: loads dist/index.html, which runs the
// feature checks and calls `report` with the JSON result; we print it and quit.
#[tauri::command]
fn report(app: tauri::AppHandle, json: String) {
    println!("PROBE_RESULT {json}");
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![report])
        .run(tauri::generate_context!())
        .expect("tauri failed");
}
