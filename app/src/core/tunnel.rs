// Viewer: TCP tunnels — a port on this computer (127.0.0.1 only) whose connections
// come out of the remote computer to host:port there (Session::open_tunnel).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

pub struct Tunnel {
    pub id: u32,
    pub tab: u32,
    pub local: u16,
    pub host: String,
    pub port: u16,
    /// connections open now / since the start
    pub open: Arc<AtomicUsize>,
    #[allow(dead_code)]
    pub total: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Core {
    /// Starts forwarding 127.0.0.1:`local` → `host`:`port` on the remote side.
    pub fn tunnel_open(&mut self, key: u32, local: &str, host: &str, port: &str) -> Result<(), String> {
        let local: u16 = local.parse().ok().filter(|p| *p > 0).ok_or_else(|| t("tun.badLocal"))?;
        let port: u16 = port.parse().ok().filter(|p| *p > 0).ok_or_else(|| t("tun.badPort"))?;
        let host = if host.is_empty() { "127.0.0.1".to_string() } else { host.to_string() };
        if host.len() > 253 || host.contains(char::is_whitespace) {
            return Err(t("tun.badHost"));
        }
        let sid = self.tabs.iter().find(|t| t.key == key).and_then(|t| t.sid);
        let s = self.session(sid).ok_or_else(|| t("tun.noSession"))?;
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", local)).map_err(|e| tf("tun.bindFailed", &[("e", &e.to_string())]))?;
        std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (open, total) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (o, n, h) = (open.clone(), total.clone(), host.clone());
        let task = crate::rt::spawn(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(std_listener) else { return };
            while let Ok((tcp, _)) = listener.accept().await {
                let (s, o, n, h) = (s.clone(), o.clone(), n.clone(), h.clone());
                crate::rt::spawn(async move {
                    match s.open_tunnel(&h, port).await {
                        Ok((tx, rx)) => {
                            n.fetch_add(1, Ordering::Relaxed);
                            o.fetch_add(1, Ordering::Relaxed);
                            crate::mdp::session::pipe(&s, tcp, tx, rx).await;
                            o.fetch_sub(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let e = if e == "denied" { t("tun.denied") } else { tf("tun.refused", &[("to", &format!("{h}:{port}"))]) };
                            bus::emit("toast", json!({ "text": e, "error": true }));
                        }
                    }
                });
            }
        });
        self.next_tunnel += 1;
        self.tunnels.push(Tunnel { id: self.next_tunnel, tab: key, local, host, port, open, total, task });
        self.toast(tf("tun.started", &[("local", &local.to_string())]), false);
        Ok(())
    }

    pub fn tunnel_close(&mut self, id: u32) {
        self.tunnels.retain(|t| t.id != id);
    }
}
