// Session actions beyond picture and input: restart the remote computer (and reconnect
// when it is back), privacy mode, special keys, lock on session end, the whiteboard and
// switching sides. Over the control stream:
//
//   viewer → host  {"t":"restart"}                      needs the Restart permission
//                  {"t":"privacy","on"}                 needs Privacy mode
//                  {"t":"special","k":"cad"|"lock"}     needs Keyboard & mouse
//                  {"t":"lockend","on"}                 lock this computer when the session ends
//                  {"t":"draw","s":id,"c":[r,g,b],"p":[[x,y],..]} / {"t":"drawclear"}
//                  {"t":"switch","token"}               "connect to me and take over my computer"
//   host → viewer  {"t":"restarting"} / {"t":"privacy","on","e"} / {"t":"notice","e"}
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::platform::system;

/// A stroke on the host's screen (normalized to the display the viewer is shown).
#[derive(Clone)]
pub struct DrawStroke {
    pub id: u64,
    pub color: [u8; 3],
    pub pts: Vec<(f32, f32)>,
}

/// Viewer: waiting for a restarted computer to come back.
pub struct Reconnect {
    pub since: Instant,
    pub deadline: Instant,
    last_ping: Option<Instant>,
    /// a ping went unanswered: the computer is down (or restarting)
    down_at: Option<Instant>,
}

const MAX_STROKES: usize = 300;
const MAX_POINTS: usize = 4000;

impl Core {
    // ------------------------------------------------------------- host side ---

    /// Returns true when `kind` was one of these actions.
    pub(super) fn host_action(&mut self, i: usize, kind: &str, m: &Value) -> bool {
        let perms = self.hosts[i].perms;
        let sid = self.hosts[i].sid;
        let deny = |core: &Core| core.ctl(sid, json!({ "t": "notice", "e": t("act.denied") }));
        match kind {
            "restart" => {
                if !perms.restart {
                    deny(self);
                    return true;
                }
                let who = self.hosts[i].req.name.clone();
                self.toast(tf("act.restartBy", &[("who", &who)]), false);
                self.ctl(sid, json!({ "t": "restarting" }));
                let reg = self.sessions.clone();
                crate::rt::spawn(async move {
                    // let the answer leave before the network goes down
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    if let Err(e) = tokio::task::spawn_blocking(system::reboot).await.unwrap_or(Err("join".into())) {
                        if let Some(s) = sid.and_then(|id| reg.lock().unwrap().get(&id).cloned()) {
                            let _ = s.send_text(false, &json!({ "t": "notice", "e": format!("{}: {e}", t("act.restartFailed")) }).to_string()).await;
                        }
                    }
                });
            }
            "privacy" => {
                let on = m["on"].as_bool().unwrap_or(false);
                if on && !perms.privacy {
                    deny(self);
                    return true;
                }
                if on && cfg!(target_os = "linux") {
                    self.ctl(sid, json!({ "t": "privacy", "on": false, "e": t("priv.unsupported") }));
                    return true;
                }
                self.hosts[i].privacy = on;
                self.update_privacy();
                self.ctl(sid, json!({ "t": "privacy", "on": on }));
            }
            "special" => {
                if !perms.control {
                    deny(self);
                    return true;
                }
                let r = match m["k"].as_str() {
                    Some("lock") => system::lock_screen(),
                    Some("cad") => system::secure_attention(),
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    let e = if e == "unsupported" { t("act.unsupported") } else { e };
                    self.ctl(sid, json!({ "t": "notice", "e": e }));
                }
            }
            "lockend" => {
                if perms.control {
                    self.hosts[i].lock_on_end = m["on"].as_bool().unwrap_or(false);
                }
            }
            "draw" => {
                if !perms.control {
                    return true;
                }
                let id = m["s"].as_u64().unwrap_or(0);
                let c = m["c"].as_array().map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0).min(255) as u8).collect::<Vec<_>>()).unwrap_or_default();
                let color = [c.first().copied().unwrap_or(229), c.get(1).copied().unwrap_or(70), c.get(2).copied().unwrap_or(59)];
                let pts: Vec<(f32, f32)> = m["p"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|p| Some((p.get(0)?.as_f64()?.clamp(0.0, 1.0) as f32, p.get(1)?.as_f64()?.clamp(0.0, 1.0) as f32))).collect())
                    .unwrap_or_default();
                let strokes = &mut self.hosts[i].strokes;
                match strokes.iter_mut().find(|s| s.id == id) {
                    Some(s) => {
                        if s.pts.len() + pts.len() <= MAX_POINTS {
                            s.pts.extend(pts);
                        }
                    }
                    None => {
                        if strokes.len() >= MAX_STROKES {
                            strokes.remove(0);
                        }
                        strokes.push(DrawStroke { id, color, pts });
                    }
                }
                bus::wake();
            }
            "drawclear" => self.hosts[i].strokes.clear(),
            "switch" => {
                // The viewer offers its own computer: connect back with its token.
                let (Some(token), from) = (m["token"].as_str().filter(|s| s.len() == 32), self.hosts[i].req.from.clone()) else { return true };
                if from.len() != 9 {
                    return true;
                }
                let who = self.hosts[i].req.name.clone();
                self.toast(tf("act.switchBy", &[("who", &who)]), false);
                self.connect(&from);
                // (the old session ends from the other side once the reversed one is live)
                if let Some(req) = self.tabs.iter_mut().rev().find(|t| t.peer_id == from).and_then(|t| t.outbox.as_mut()) {
                    req["switch"] = json!(token);
                }
            }
            _ => return false,
        }
        true
    }

    /// Privacy mode is on while any live viewer asked for it: the screens here go black
    /// (the overlay is kept out of the shared picture) and the local input is blocked.
    pub(super) fn update_privacy(&mut self) {
        let on = self.hosts.iter().any(|h| h.state == HostState::Live && h.privacy);
        if on == self.privacy_on {
            return;
        }
        self.privacy_on = on;
        self.input.block(on);
        if !on {
            self.update_host_input();
            return;
        }
        crate::rt::spawn(async {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            bus::emit("privacy:check", json!({}));
        });
    }

    /// A second after privacy mode starts: tell the viewers if the input stayed unblocked.
    pub(super) fn privacy_check(&mut self) {
        if !self.privacy_on || self.input.blocked() {
            return;
        }
        let sids: Vec<_> = self.hosts.iter().filter(|h| h.privacy).map(|h| h.sid).collect();
        for sid in sids {
            self.ctl(sid, json!({ "t": "notice", "e": t("priv.noBlock") }));
        }
    }

    /// Host session over: lock if the viewer asked for it.
    pub(super) fn on_host_closed(&mut self, h: &Host) {
        if h.lock_on_end && h.live_since.is_some() {
            if let Err(e) = system::lock_screen() {
                eprintln!("[lockend] {e}");
            }
        }
        self.update_privacy();
    }

    /// A request from the computer we offered ours to (switch sides): accepted as is.
    pub(super) fn is_switch_request(&mut self, msg: &Value) -> bool {
        let Some((token, tab_key, at)) = self.switch_offer.clone() else { return false };
        let from = msg["from"].as_str().unwrap_or("");
        let ok = msg["switch"].as_str() == Some(token.as_str()) && at.elapsed() < Duration::from_secs(120) && self.tabs.iter().any(|t| t.key == tab_key && t.peer_id == from);
        if ok {
            self.switch_offer = None;
            self.switch_close_tab = Some(tab_key);
        }
        ok
    }

    /// Switch sides finished on our end: the reversed session is live, the old one goes.
    pub(super) fn switch_live(&mut self) {
        if let Some(k) = self.switch_close_tab.take() {
            self.close_tab(k);
        }
    }

    // ----------------------------------------------------------- viewer side ---

    /// Answers from the host to the actions above. True if handled.
    pub(super) fn viewer_action(&mut self, i: usize, m: &Value) -> bool {
        let label = self.tabs[i].label.clone();
        match m["t"].as_str() {
            Some("restarting") => {
                let now = Instant::now();
                self.tabs[i].reconnect = Some(Reconnect { since: now, deadline: now + Duration::from_secs(15 * 60), last_ping: None, down_at: None });
                self.toast(format!("{label}: {}", t("act.restarting")), false);
            }
            Some("privacy") => {
                self.tabs[i].privacy = m["on"].as_bool().unwrap_or(false);
                if let Some(e) = m["e"].as_str().filter(|e| !e.is_empty()) {
                    self.toast(format!("{label}: {e}"), true);
                }
            }
            Some("nopicture") => {
                eprintln!("[viewer] the host reports it can't capture its screen");
                let msg = if m["os"] == "darwin" { t("cap.noPictureMac") } else { t("cap.noPicture") };
                self.tabs[i].no_picture = Some(msg);
            }
            Some("notice") => {
                let e = m["e"].as_str().unwrap_or("").to_string();
                self.toast(format!("{label}: {e}"), true);
            }
            _ => return false,
        }
        true
    }

    pub fn restart_remote(&mut self, key: u32) {
        if let Some(sid) = self.tab(key).map(|t| t.sid) {
            self.ctl(sid, json!({ "t": "restart" }));
        }
    }

    pub fn set_privacy(&mut self, key: u32, on: bool) {
        if let Some(sid) = self.tab(key).map(|t| t.sid) {
            self.ctl(sid, json!({ "t": "privacy", "on": on }));
        }
    }

    pub fn set_lock_on_end(&mut self, key: u32, on: bool) {
        let Some(tab) = self.tab(key) else { return };
        tab.lock_on_end = on;
        let sid = tab.sid;
        self.ctl(sid, json!({ "t": "lockend", "on": on }));
    }

    /// "cad" / "lock" are done by the host; key combinations go as plain key events.
    pub fn special(&mut self, key: u32, what: &str) {
        let Some(tab) = self.tab(key) else { return };
        let sid = tab.sid;
        match what {
            "cad" | "lock" => self.ctl(sid, json!({ "t": "special", "k": what })),
            _ => {}
        }
    }

    /// Presses `codes` together (KeyboardEvent.code names) and releases them in reverse.
    pub fn send_combo(&mut self, key: u32, codes: &[&str]) {
        for c in codes {
            self.input(key, json!({ "t": "key", "code": c, "d": true }));
        }
        for c in codes.iter().rev() {
            self.input(key, json!({ "t": "key", "code": c, "d": false }));
        }
    }

    pub fn send_draw(&mut self, key: u32, stroke: u64, color: [u8; 3], pts: &[(f32, f32)]) {
        if pts.is_empty() {
            return;
        }
        let Some(sid) = self.tab(key).map(|t| t.sid) else { return };
        let p: Vec<[f32; 2]> = pts.iter().map(|(x, y)| [(x * 10000.0).round() / 10000.0, (y * 10000.0).round() / 10000.0]).collect();
        self.ctl(sid, json!({ "t": "draw", "s": stroke, "c": color, "p": p }));
    }

    pub fn draw_clear(&mut self, key: u32) {
        if let Some(sid) = self.tab(key).map(|t| t.sid) {
            self.ctl(sid, json!({ "t": "drawclear" }));
        }
    }

    /// Offers this computer to the other side: it connects back and takes over.
    pub fn switch_sides(&mut self, key: u32) {
        let Some(sid) = self.tab(key).map(|t| t.sid) else { return };
        let token = hex::encode(rand::random::<[u8; 16]>());
        self.switch_offer = Some((token.clone(), key, Instant::now()));
        self.ctl(sid, json!({ "t": "switch", "token": token }));
        self.toast(t("act.switching"), false);
    }

    /// Viewer: a restarted computer — ping it until it answers again, then reconnect.
    pub(super) fn tick_reconnect(&mut self) {
        let now = Instant::now();
        let mut again = Vec::new();
        let mut ping = Vec::new();
        for tab in self.tabs.iter_mut() {
            if tab.state == TabState::Live {
                tab.reconnect = None;
            }
            let Some(r) = tab.reconnect.as_mut() else { continue };
            if !matches!(tab.state, TabState::Failed(_)) {
                continue;
            }
            if now > r.deadline {
                tab.reconnect = None;
                tab.state = TabState::Failed(t("act.reconnectFailed"));
                continue;
            }
            let seen = self.presence.get(&tab.peer_id).map(|p| p.0);
            if let Some(last) = r.last_ping {
                if r.down_at.is_none() && now.duration_since(last) > Duration::from_secs(8) && seen.is_none_or(|s| s < last) {
                    r.down_at = Some(last);
                }
            }
            let back = r.down_at.is_some_and(|d| seen.is_some_and(|s| s > d));
            // a host that doesn't answer pings at all: just try now and then
            let blind = now.duration_since(r.since) > Duration::from_secs(180) && r.down_at.is_some() && r.last_ping.is_some_and(|l| now.duration_since(l) > Duration::from_secs(55));
            if back || blind {
                again.push(tab.key);
            } else if r.last_ping.is_none_or(|l| now.duration_since(l) > Duration::from_secs(10)) && now.duration_since(r.since) > Duration::from_secs(5) {
                r.last_ping = Some(now);
                ping.push(tab.peer_id.clone());
            }
        }
        if !ping.is_empty() {
            self.ping_ids(ping);
        }
        for key in again {
            let Some(tab) = self.tabs.iter().find(|t| t.key == key) else { continue };
            let (id, deadline, since) = (tab.peer_id.clone(), tab.reconnect.as_ref().map(|r| r.deadline), tab.reconnect.as_ref().map(|r| r.since));
            self.close_tab(key);
            self.connect(&id);
            // keep waiting if this attempt fails too (the computer may still be starting)
            if let (Some(tab), Some(deadline), Some(since)) = (self.tabs.iter_mut().rev().find(|t| t.peer_id == id), deadline, since) {
                tab.reconnect = Some(Reconnect { since, deadline, last_ping: Some(Instant::now()), down_at: Some(Instant::now()) });
            }
        }
    }
}
