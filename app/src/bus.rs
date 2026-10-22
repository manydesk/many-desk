// Core → UI events: a name plus JSON (the same shapes the old web UI received), and raw
// media bytes per session. The UI drains them every frame.
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

pub type Emit = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

pub enum Msg {
    Event(&'static str, Value),
    /// binary file-transfer chunk for session `sid`
    File(u32, Vec<u8>),
}

static TX: OnceLock<Mutex<Sender<Msg>>> = OnceLock::new();
static WAKE: OnceLock<egui::Context> = OnceLock::new();

pub fn init() -> Receiver<Msg> {
    let (tx, rx) = channel();
    let _ = TX.set(Mutex::new(tx));
    rx
}

/// Lets core threads wake the UI when something arrives.
pub fn set_waker(ctx: egui::Context) {
    let _ = WAKE.set(ctx);
}

pub fn wake() {
    if let Some(c) = WAKE.get() {
        c.request_repaint();
    }
}

pub fn emit(name: &'static str, v: Value) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::Event(name, v));
    }
    wake();
}

pub fn file(sid: u32, data: Vec<u8>) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::File(sid, data));
    }
    wake();
}

pub fn emitter() -> Emit {
    Arc::new(emit)
}
