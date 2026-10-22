// Test hook: MANYDESK_SIM="delay_ms,kbps,buffer_ms[,jitter_ms]" puts a slow link under this end's
// QUIC socket — every outgoing datagram waits `delay_ms` (one way), the link sends
// `kbps` (0 = unlimited) and holds at most `buffer_ms` of queue (more is dropped, like a
// router's buffer); `jitter_ms` adds a random 0…jitter to each packet, as on Wi-Fi.
// Set it on both ends for a round trip of twice the delay.
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};

pub struct Sim {
    delay: Duration,
    bytes_per_sec: f64,
    buffer: Duration,
    jitter: Duration,
}

impl Sim {
    pub fn from_env() -> Option<Sim> {
        let v = std::env::var("MANYDESK_SIM").ok().filter(|v| !v.trim().is_empty())?;
        let n: Vec<f64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        let (d, k, b, j) = (n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0), n.get(2).copied().unwrap_or(200.0), n.get(3).copied().unwrap_or(0.0));
        eprintln!("[sim] link: {d} ms one way, {k} kbit/s, {b} ms buffer, {j} ms jitter");
        Some(Sim { delay: Duration::from_secs_f64(d / 1000.0), bytes_per_sec: k * 1000.0 / 8.0, buffer: Duration::from_secs_f64(b / 1000.0), jitter: Duration::from_secs_f64(j / 1000.0) })
    }
}

pub struct SimSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    sim: Sim,
    /// when the link is free again
    free_at: Mutex<Instant>,
    /// the last packet's delivery time: jitter delays packets but keeps their order
    last_out: Mutex<Instant>,
}

impl fmt::Debug for SimSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SimSocket")
    }
}

impl SimSocket {
    pub fn wrap(inner: Arc<dyn AsyncUdpSocket>, sim: Sim) -> Arc<dyn AsyncUdpSocket> {
        Arc::new(SimSocket { inner, sim, free_at: Mutex::new(Instant::now()), last_out: Mutex::new(Instant::now()) })
    }
}

impl AsyncUdpSocket for SimSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, t: &Transmit) -> io::Result<()> {
        let now = Instant::now();
        // serialization on the bottleneck
        let release = {
            let mut free = self.free_at.lock().unwrap();
            let start = (*free).max(now);
            if self.sim.bytes_per_sec > 0.0 {
                if start.duration_since(now) > self.sim.buffer {
                    return Ok(()); // queue full: the packet is lost
                }
                *free = start + Duration::from_secs_f64(t.contents.len() as f64 / self.sim.bytes_per_sec);
            }
            let mut last = self.last_out.lock().unwrap();
            *last = (*free + self.sim.delay + self.sim.jitter.mul_f64(rand::random::<f64>())).max(*last);
            *last
        };
        let (dest, ecn, data, seg, src) = (t.destination, t.ecn, t.contents.to_vec(), t.segment_size, t.src_ip);
        let inner = self.inner.clone();
        crate::rt::spawn(async move {
            tokio::time::sleep_until(release.into()).await;
            let _ = inner.try_send(&Transmit { destination: dest, ecn, contents: &data, segment_size: seg, src_ip: src });
        });
        Ok(())
    }

    fn poll_recv(&self, cx: &mut Context, bufs: &mut [IoSliceMut<'_>], meta: &mut [RecvMeta]) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}
