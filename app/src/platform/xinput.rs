// Remote-control input on Linux/X11 via the XTEST extension, in-process (macOS and
// Windows use a helper executable for the same JSON commands, see input.rs).
// Wayland has no equivalent without the RemoteDesktop portal; there this is a no-op.
use std::collections::HashSet;

use serde_json::Value;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

pub struct XInput {
    conn: RustConnection,
    root: u32,
    keys: HashSet<u8>,
    buttons: HashSet<u8>,
    // Browser wheel deltas are pixels (~100 per notch); X scrolls in clicks.
    acc_x: f64,
    acc_y: f64,
}

impl XInput {
    pub fn new() -> Option<Self> {
        if crate::media::capture::wayland() {
            return None;
        }
        let (conn, num) = x11rb::connect(None).ok()?;
        conn.xtest_get_version(2, 2).ok()?.reply().ok()?;
        let root = conn.setup().roots[num].root;
        Some(XInput { conn, root, keys: HashSet::new(), buttons: HashSet::new(), acc_x: 0.0, acc_y: 0.0 })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) {
        let _ = self.conn.xtest_fake_input(kind, detail, 0, self.root, x, y, 0);
    }

    fn click(&self, button: u8) {
        self.fake(BUTTON_PRESS_EVENT, button, 0, 0);
        self.fake(BUTTON_RELEASE_EVENT, button, 0, 0);
    }

    pub fn handle(&mut self, c: &Value) {
        let num = |k: &str| c[k].as_f64().unwrap_or(0.0);
        let (x, y) = (num("x").round() as i16, num("y").round() as i16);
        // 0 left, 1 middle, 2 right → X buttons 1, 2, 3
        let button = (c["b"].as_u64().unwrap_or(0).min(2) + 1) as u8;
        match c["t"].as_str().unwrap_or("") {
            "move" => self.fake(MOTION_NOTIFY_EVENT, 0, x, y),
            "down" => {
                self.fake(MOTION_NOTIFY_EVENT, 0, x, y);
                self.fake(BUTTON_PRESS_EVENT, button, 0, 0);
                self.buttons.insert(button);
            }
            "up" => {
                self.fake(MOTION_NOTIFY_EVENT, 0, x, y);
                self.fake(BUTTON_RELEASE_EVENT, button, 0, 0);
                self.buttons.remove(&button);
            }
            "scroll" => {
                self.acc_y += num("dy");
                self.acc_x += num("dx");
                while self.acc_y >= 50.0 { self.click(5); self.acc_y -= 50.0; }
                while self.acc_y <= -50.0 { self.click(4); self.acc_y += 50.0; }
                while self.acc_x >= 50.0 { self.click(7); self.acc_x -= 50.0; }
                while self.acc_x <= -50.0 { self.click(6); self.acc_x += 50.0; }
            }
            "key" => {
                let Some(k) = c["k"].as_u64().map(|k| k as u8) else { return };
                if c["d"].as_bool().unwrap_or(false) {
                    self.fake(KEY_PRESS_EVENT, k, 0, 0);
                    self.keys.insert(k);
                } else {
                    self.fake(KEY_RELEASE_EVENT, k, 0, 0);
                    self.keys.remove(&k);
                }
            }
            "release" => {
                for k in std::mem::take(&mut self.keys) {
                    self.fake(KEY_RELEASE_EVENT, k, 0, 0);
                }
                for b in std::mem::take(&mut self.buttons) {
                    self.fake(BUTTON_RELEASE_EVENT, b, 0, 0);
                }
            }
            "text" => self.type_text(c["s"].as_str().unwrap_or("")),
            _ => {}
        }
        let _ = self.conn.flush();
    }

    /// Types text whatever the keyboard layout: each character is mapped onto a spare
    /// keycode for the moment it is pressed (the usual XTEST trick), then the map is restored.
    fn type_text(&mut self, text: &str) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        use x11rb::wrapper::ConnectionExt as _;
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let Ok(Ok(map)) = self.conn.get_keyboard_mapping(min, max - min + 1).map(|c| c.reply()) else { return };
        let per = map.keysyms_per_keycode as usize;
        // a keycode with nothing on it
        let Some(spare) = (0..=(max - min) as usize).rev().find(|&i| map.keysyms[i * per..(i + 1) * per].iter().all(|&k| k == 0)).map(|i| min + i as u8) else { return };
        for ch in text.chars() {
            let cp = ch as u32;
            let sym = match cp {
                0x0A | 0x0D => 0xff0d, // Return
                0x09 => 0xff09,        // Tab
                0x20..=0x7e | 0xa0..=0xff => cp,
                _ => 0x0100_0000 | cp, // Unicode keysym
            };
            let syms = vec![sym; per.max(1)];
            if self.conn.change_keyboard_mapping(1, spare, per.max(1) as u8, &syms).is_err() {
                return;
            }
            let _ = self.conn.sync();
            self.fake(KEY_PRESS_EVENT, spare, 0, 0);
            self.fake(KEY_RELEASE_EVENT, spare, 0, 0);
            let _ = self.conn.sync();
        }
        let _ = self.conn.change_keyboard_mapping(1, spare, per.max(1) as u8, &vec![0; per.max(1)]);
    }
}
