// The remote terminal (RustDesk's terminal session): a shell on the host, its screen on the
// viewer (vt100 keeps the screen there). Over the control stream, bytes in base64:
//
//   viewer → host  {"t":"term","op":"open","id","cols","rows"}   needs the Terminal permission
//                  {"t":"term","op":"in","id","d"}                keys typed
//                  {"t":"term","op":"size","id","cols","rows"}
//                  {"t":"term","op":"close","id"}
//   host → viewer  {"t":"term","op":"out","id","d"}               what the shell printed
//                  {"t":"term","op":"exit","id","e"}              it ended ("e": why, if it failed)
//
// Both directions go through one queue per terminal (Ordered): Core::ctl sends each
// message from a task of its own, and keys or output must not overtake each other.
#[cfg(not(mobile))]
use std::io::{Read, Write};

use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use super::*;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
/// Lines kept above the screen (the wheel scrolls through them).
const SCROLLBACK: usize = 5000;

/// Messages to the other side, sent one after another.
#[derive(Clone)]
pub struct Ordered(UnboundedSender<String>);

impl Ordered {
    fn new(s: Arc<Session>) -> Self {
        let (tx, mut rx) = unbounded_channel::<String>();
        crate::rt::spawn(async move {
            while let Some(text) = rx.recv().await {
                if s.send_text(false, &text).await.is_err() {
                    break;
                }
            }
        });
        Ordered(tx)
    }

    fn send(&self, msg: Value) {
        let _ = self.0.send(msg.to_string());
    }
}

/// Host: one shell.
pub struct HostTerm {
    #[cfg(not(mobile))]
    writer: Box<dyn Write + Send>,
    #[cfg(not(mobile))]
    master: Box<dyn portable_pty::MasterPty + Send>,
    #[cfg(not(mobile))]
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Drop for HostTerm {
    fn drop(&mut self) {
        #[cfg(not(mobile))]
        let _ = self.child.kill();
    }
}

/// Viewer: the terminal window of a session.
pub struct TermView {
    pub id: u32,
    pub parser: vt100::Parser,
    /// the shell ended (with the reason when it failed to start)
    pub ended: Option<String>,
    pub size: (u16, u16),
    tx: Ordered,
}

impl TermView {
    pub fn send_keys(&self, bytes: &[u8]) {
        if self.ended.is_none() && !bytes.is_empty() {
            self.tx.send(json!({ "t": "term", "op": "in", "id": self.id, "d": B64.encode(bytes) }));
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) != self.size && rows > 1 && cols > 1 {
            self.size = (rows, cols);
            self.parser.screen_mut().set_size(rows, cols);
            self.tx.send(json!({ "t": "term", "op": "size", "id": self.id, "cols": cols, "rows": rows }));
        }
    }
}

#[cfg(not(mobile))]
fn spawn_shell(rows: u16, cols: u16) -> Result<(HostTerm, Box<dyn Read + Send>), String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
    let mut cmd = if cfg!(windows) { CommandBuilder::new("powershell.exe") } else { CommandBuilder::new_default_prog() };
    if !cfg!(windows) {
        // a login shell, as a terminal app starts it (the user's PATH and profile)
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
    }
    cmd.env("TERM", "xterm-256color");
    if let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()) {
        cmd.cwd(home);
    }
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    Ok((HostTerm { writer, master: pair.master, child }, reader))
}

impl Core {
    // ------------------------------------------------------------- host side ---

    pub(super) fn host_term(&mut self, i: usize, m: &Value) {
        let sid = self.hosts[i].sid;
        let id = m["id"].as_u64().unwrap_or(0) as u32;
        let size = |m: &Value| (m["rows"].as_u64().unwrap_or(24).clamp(2, 500) as u16, m["cols"].as_u64().unwrap_or(80).clamp(2, 1000) as u16);
        match m["op"].as_str() {
            Some("open") => {
                if !self.hosts[i].perms.terminal {
                    return self.ctl(sid, json!({ "t": "term", "op": "exit", "id": id, "e": t("act.denied") }));
                }
                let Some(s) = self.session(sid) else { return };
                let out = Ordered::new(s);
                #[cfg(mobile)]
                {
                    let _ = size;
                    out.send(json!({ "t": "term", "op": "exit", "id": id, "e": t("term.unsupported") }));
                }
                #[cfg(not(mobile))]
                {
                    let (rows, cols) = size(m);
                    match spawn_shell(rows, cols) {
                        Ok((term, mut reader)) => {
                            self.hosts[i].terms.insert(id, term);
                            let who = self.hosts[i].req.name.clone();
                            self.toast(tf("term.openedBy", &[("who", &who)]), false);
                            std::thread::Builder::new()
                                .name("term".into())
                                .spawn(move || {
                                    let mut buf = vec![0u8; 32 * 1024];
                                    loop {
                                        match reader.read(&mut buf) {
                                            Ok(0) | Err(_) => break,
                                            Ok(n) => out.send(json!({ "t": "term", "op": "out", "id": id, "d": B64.encode(&buf[..n]) })),
                                        }
                                    }
                                    out.send(json!({ "t": "term", "op": "exit", "id": id }));
                                })
                                .ok();
                        }
                        Err(e) => {
                            eprintln!("[term] {e}");
                            out.send(json!({ "t": "term", "op": "exit", "id": id, "e": e }));
                        }
                    }
                }
            }
            #[cfg(not(mobile))]
            Some("in") => {
                if let (Some(term), Ok(d)) = (self.hosts[i].terms.get_mut(&id), B64.decode(m["d"].as_str().unwrap_or(""))) {
                    let _ = term.writer.write_all(&d);
                    let _ = term.writer.flush();
                }
            }
            #[cfg(not(mobile))]
            Some("size") => {
                if let Some(term) = self.hosts[i].terms.get(&id) {
                    let (rows, cols) = size(m);
                    let _ = term.master.resize(portable_pty::PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                }
            }
            Some("close") => {
                self.hosts[i].terms.remove(&id);
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------- viewer side ---

    pub(super) fn viewer_term(&mut self, i: usize, m: &Value) {
        let id = m["id"].as_u64().unwrap_or(0) as u32;
        let Some(term) = self.tabs[i].term.as_mut().filter(|t| t.id == id) else { return };
        match m["op"].as_str() {
            Some("out") => {
                if let Ok(d) = B64.decode(m["d"].as_str().unwrap_or("")) {
                    term.parser.process(&d);
                }
            }
            Some("exit") => term.ended = Some(m["e"].as_str().unwrap_or("").to_string()),
            _ => {}
        }
    }

    /// Opens (or brings back) the terminal of session `key`.
    pub fn term_open(&mut self, key: u32, rows: u16, cols: u16) {
        let Some(tab) = self.tab(key) else { return };
        if tab.term.as_ref().is_some_and(|t| t.ended.is_none()) {
            return;
        }
        let sid = tab.sid;
        let Some(s) = self.session(sid) else { return };
        let id = rand::random::<u32>() >> 1;
        let tx = Ordered::new(s);
        tx.send(json!({ "t": "term", "op": "open", "id": id, "cols": cols, "rows": rows }));
        let tab = self.tab(key).unwrap();
        tab.term = Some(TermView { id, parser: vt100::Parser::new(rows, cols, SCROLLBACK), ended: None, size: (rows, cols), tx });
        tab.term_open = true;
    }

    /// Closes the terminal window; the shell ends with it.
    pub fn term_close(&mut self, key: u32) {
        let Some(tab) = self.tab(key) else { return };
        tab.term_open = false;
        if let Some(term) = tab.term.take() {
            if term.ended.is_none() {
                term.tx.send(json!({ "t": "term", "op": "close", "id": term.id }));
            }
        }
    }
}
