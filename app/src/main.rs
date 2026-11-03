// ManyDesk desktop app; everything lives in the library (src/lib.rs), shared with Android.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    manydesk::run()
}
