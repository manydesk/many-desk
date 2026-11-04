// The remote display's resolution (Actions → Resolution, as in AnyDesk and RustDesk). Over
// the control stream:
//
//   viewer → host  {"t":"modes"}                         what the display it is shown offers
//                  {"t":"setmode","w","h","hz"}           switch to it (Keyboard & mouse permission)
//                  {"t":"setmode","orig":true}            back to what it was
//                  {"t":"vdisplay","on","w","h"}          add (and show) or remove a virtual display
//   host → viewer  {"t":"modes","screen","list":[[w,h,hz]…],"cur":[w,h,hz],"changed","virtual","vd"}
//
// The host keeps each display's mode from before the first change and puts it back when
// the last session ends (and macOS on its own when ManyDesk quits).
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::platform::display::{self, Mode};

/// What the viewer knows of the host's display modes.
#[derive(Clone, Default)]
pub struct RemoteModes {
    pub screen: u32,
    pub list: Vec<Mode>,
    pub cur: Option<Mode>,
    pub changed: bool,
    /// the host can add a virtual display, and has one now
    pub virtual_ok: bool,
    pub vd_on: bool,
    /// asked, no answer yet
    pub asked: bool,
}

fn mode_json(m: &Mode) -> Value {
    json!([m.w, m.h, m.hz])
}

fn mode_of(v: &Value) -> Option<Mode> {
    let a = v.as_array()?;
    Some(Mode { w: a.first()?.as_u64()? as u32, h: a.get(1)?.as_u64()? as u32, hz: a.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as u32 })
}

impl Core {
    // ------------------------------------------------------------- host side ---

    pub(super) fn host_modes(&mut self, i: usize) {
        let Some(screen) = self.hosts[i].screen.as_ref().map(|s| s.id) else { return };
        let list: Vec<Value> = display::modes(screen).iter().map(mode_json).collect();
        let cur = display::current(screen).map(|m| mode_json(&m));
        let changed = self.mode_orig.contains_key(&screen);
        let (ok, on) = (crate::platform::vdisplay::SUPPORTED, self.vdisplay.is_some());
        self.ctl(self.hosts[i].sid, json!({ "t": "modes", "screen": screen, "list": list, "cur": cur, "changed": changed, "virtual": ok, "vd": on }));
    }

    pub(super) fn host_vdisplay(&mut self, i: usize, m: &Value) {
        let sid = self.hosts[i].sid;
        if !self.hosts[i].perms.control {
            return self.ctl(sid, json!({ "t": "notice", "e": t("act.denied") }));
        }
        let on = m["on"].as_bool().unwrap_or(false);
        if on == self.vdisplay.is_some() {
            return;
        }
        if on {
            let even = |v: &Value, d: u64| (v.as_u64().unwrap_or(d).clamp(800, 3840) as u32) & !1;
            match crate::platform::vdisplay::create(even(&m["w"], 1920), even(&m["h"], 1080)) {
                Ok(vd) => {
                    let id = vd.id;
                    self.vdisplay = Some(vd);
                    let who = self.hosts[i].req.name.clone();
                    self.toast(tf("vd.addedBy", &[("who", &who)]), false);
                    // this viewer goes to it (once the system has it in its list)
                    self.show_display(vec![(self.hosts[i].key, sid)], id);
                }
                Err(e) => {
                    eprintln!("[vdisplay] {e}");
                    self.ctl(sid, json!({ "t": "notice", "e": t("vd.failed") }));
                }
            }
        } else {
            self.drop_vdisplay();
        }
    }

    /// Sessions showing the virtual display go to another one, then it goes.
    fn drop_vdisplay(&mut self) {
        let Some(vd) = self.vdisplay.take() else { return };
        let other = crate::media::capture::screens(false).into_iter().find(|s| s.id != vd.id).map(|s| s.id);
        let on_it: Vec<(u32, Option<u32>)> = self.hosts.iter().filter(|h| h.screen.as_ref().is_some_and(|s| s.id == vd.id)).map(|h| (h.key, h.sid)).collect();
        drop(vd);
        if let Some(id) = other {
            self.show_display(on_it, id);
        }
    }

    /// These host sessions show display `screen` from now on (after the display list settled).
    fn show_display(&mut self, hosts: Vec<(u32, Option<u32>)>, screen: u32) {
        for (key, sid) in hosts {
            let Some(s) = self.session(sid) else { continue };
            crate::rt::spawn(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                match s.switch_screen(screen).await {
                    Some(sc) => bus::emit("host:screen", json!({ "host": key, "screen": serde_json::to_value(&sc).unwrap_or_default() })),
                    None => bus::emit("host:screen", json!({ "host": key })),
                }
            });
        }
    }

    pub(super) fn host_setmode(&mut self, i: usize, m: &Value) {
        let sid = self.hosts[i].sid;
        if !self.hosts[i].perms.control {
            return self.ctl(sid, json!({ "t": "notice", "e": t("act.denied") }));
        }
        let Some(screen) = self.hosts[i].screen.as_ref().map(|s| s.id) else { return };
        let want = if m["orig"].as_bool() == Some(true) {
            match self.mode_orig.get(&screen) {
                Some(o) => *o,
                None => return,
            }
        } else {
            let Some(w) = mode_of(&json!([m["w"], m["h"], m["hz"]])) else { return };
            w
        };
        if !self.mode_orig.contains_key(&screen) {
            if let Some(cur) = display::current(screen) {
                self.mode_orig.insert(screen, cur);
            }
        }
        match display::set(screen, want) {
            Ok(()) => {
                if self.mode_orig.get(&screen) == Some(&want) {
                    self.mode_orig.remove(&screen);
                }
                let who = self.hosts[i].req.name.clone();
                self.toast(tf("res.changedBy", &[("who", &who), ("w", &want.w.to_string()), ("h", &want.h.to_string())]), false);
                self.after_mode_change(screen);
            }
            Err(e) => {
                eprintln!("[display] {screen}: {e}");
                self.ctl(sid, json!({ "t": "notice", "e": t("res.failed") }));
            }
        }
    }

    /// The display's size changed: every session showing it starts its picture again at the
    /// new size (and gets the new mode list).
    fn after_mode_change(&mut self, screen: u32) {
        // the display takes a moment to settle at the new mode
        let hosts: Vec<(u32, Option<u32>)> = self.hosts.iter().filter(|h| h.state == HostState::Live && h.screen.as_ref().is_some_and(|s| s.id == screen)).map(|h| (h.key, h.sid)).collect();
        self.show_display(hosts, screen);
    }

    /// No session left: every display gets its own mode back, the virtual one goes.
    pub(super) fn restore_modes(&mut self) {
        if self.hosts.iter().any(|h| h.state == HostState::Live) {
            return;
        }
        self.vdisplay = None;
        for (screen, mode) in std::mem::take(&mut self.mode_orig) {
            if let Err(e) = display::set(screen, mode) {
                eprintln!("[display] restoring {screen}: {e}");
            }
        }
    }

    // ----------------------------------------------------------- viewer side ---

    pub(super) fn viewer_modes(&mut self, i: usize, m: &Value) {
        let list = m["list"].as_array().map(|a| a.iter().filter_map(mode_of).collect()).unwrap_or_default();
        let flag = |k: &str| m[k].as_bool().unwrap_or(false);
        self.tabs[i].modes = Some(RemoteModes { screen: m["screen"].as_u64().unwrap_or(0) as u32, list, cur: mode_of(&m["cur"]), changed: flag("changed"), virtual_ok: flag("virtual"), vd_on: flag("vd"), asked: false });
    }

    /// The menu opened: ask for the modes of the display shown (once per display).
    pub fn want_modes(&mut self, key: u32) {
        let Some(tab) = self.tab(key) else { return };
        let screen = tab.screen.unwrap_or(0);
        if tab.modes.as_ref().is_some_and(|m| m.asked || m.screen == screen) {
            return;
        }
        tab.modes = Some(RemoteModes { screen, asked: true, ..Default::default() });
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "modes" }));
    }

    /// A virtual display on the host, the size of this window's picture area.
    pub fn set_vdisplay(&mut self, key: u32, on: bool) {
        let Some(tab) = self.tab(key) else { return };
        let (sid, (w, h)) = (tab.sid, tab.sent_view);
        tab.modes = None;
        self.ctl(sid, json!({ "t": "vdisplay", "on": on, "w": w, "h": h }));
    }

    pub fn set_mode(&mut self, key: u32, mode: Option<Mode>) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        // asked again once the picture comes back at the new size
        tab.modes = None;
        match mode {
            Some(m) => self.ctl(sid, json!({ "t": "setmode", "w": m.w, "h": m.h, "hz": m.hz })),
            None => self.ctl(sid, json!({ "t": "setmode", "orig": true })),
        }
    }
}
