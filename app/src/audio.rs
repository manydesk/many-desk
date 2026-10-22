// Voice: microphone → Opus (48 kHz mono, 20 ms, 32 kbit/s, DTX) → MDP datagrams, and
// back: Opus → a small jitter buffer → speakers. Audio devices run on their own
// threads (cpal streams are not Send everywhere); the session only sees packets.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audiopus_sys as op;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const RATE: u32 = 48_000;
const FRAME: usize = 960; // 20 ms

/// Linear resampler for one mono stream (keeps its phase across calls).
struct Resampler {
    ratio: f64, // output rate / input rate
    pos: f64,
    last: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Resampler { ratio: to as f64 / from as f64, pos: 0.0, last: 0.0 }
    }

    fn run(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.ratio - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        // Positions run from -1 (the previous call's last sample) to len-1.
        let at = |i: isize, last: f32| if i < 0 { last } else { input[i as usize] };
        let step = 1.0 / self.ratio;
        let end = input.len() as f64 - 1.0;
        while self.pos <= end {
            let i = self.pos.floor() as isize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i, self.last), at((i + 1).min(end as isize), self.last));
            out.push(a + (b - a) * frac);
            self.pos += step;
        }
        self.pos -= input.len() as f64;
        self.last = *input.last().unwrap_or(&self.last);
    }
}

// ---------------------------------------------------------------- playback ---

pub struct Player {
    tx: SyncSender<Vec<u8>>,
    pub muted: Arc<AtomicBool>,
}

impl Player {
    pub fn start() -> Player {
        let (tx, rx) = sync_channel::<Vec<u8>>(64);
        let muted = Arc::new(AtomicBool::new(false));
        let m = muted.clone();
        std::thread::Builder::new().name("audio-out".into()).spawn(move || play(rx, m)).ok();
        Player { tx, muted }
    }

    /// One Opus packet from the peer.
    pub fn push(&self, packet: Vec<u8>) {
        if std::env::var_os("MANYDESK_DEBUG").is_some() {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n % 50 == 0 {
                eprintln!("[audio] received {} packets ({} bytes last)", n + 1, packet.len());
            }
        }
        let _ = self.tx.try_send(packet);
    }
}

fn play(rx: Receiver<Vec<u8>>, muted: Arc<AtomicBool>) {
    let host = cpal::default_host();
    let Some(dev) = host.default_output_device() else { return };
    let Ok(cfg) = dev.default_output_config() else { return };
    let rate = cfg.sample_rate();
    let channels = cfg.channels() as usize;
    let ring: Arc<Mutex<(VecDeque<f32>, bool)>> = Arc::new(Mutex::new((VecDeque::new(), false)));
    let r = ring.clone();
    let start_at = (rate as usize) * 60 / 1000; // 60 ms before playing
    let fill = move |data: &mut [f32]| {
        let mut g = r.lock().unwrap();
        let (buf, started) = &mut *g;
        if !*started && buf.len() >= start_at {
            *started = true;
        }
        let mute = muted.load(Ordering::Relaxed);
        for frame in data.chunks_mut(channels) {
            let s = if *started { buf.pop_front().unwrap_or_else(|| { *started = false; 0.0 }) } else { 0.0 };
            frame.fill(if mute { 0.0 } else { s });
        }
    };
    let sc: cpal::StreamConfig = cfg.config();
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_output_stream::<f32, _, _>(sc, move |d, _| fill(d), |_| {}, None),
        cpal::SampleFormat::I16 => {
            let mut tmp = Vec::new();
            dev.build_output_stream::<i16, _, _>(
                sc,
                move |d: &mut [i16], _| {
                    tmp.resize(d.len(), 0.0);
                    fill(&mut tmp);
                    for (o, s) in d.iter_mut().zip(&tmp) {
                        *o = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                |_| {},
                None,
            )
        }
        _ => return,
    };
    let Ok(stream) = stream else { return };
    let _ = stream.play();
    let mut err = 0;
    let dec = unsafe { op::opus_decoder_create(RATE as i32, 1, &mut err) };
    if dec.is_null() {
        return;
    }
    let mut pcm = vec![0f32; FRAME * 6];
    let mut rs = Resampler::new(RATE, rate);
    let mut out = Vec::new();
    let cap = rate as usize / 5; // 200 ms
    while let Ok(pkt) = rx.recv() {
        let n = unsafe { op::opus_decode_float(dec, pkt.as_ptr(), pkt.len() as i32, pcm.as_mut_ptr(), pcm.len() as i32, 0) };
        if n <= 0 {
            continue;
        }
        out.clear();
        rs.run(&pcm[..n as usize], &mut out);
        let mut g = ring.lock().unwrap();
        g.0.extend(out.iter().copied());
        // Stay close to real time: drop the oldest audio above 200 ms.
        while g.0.len() > cap {
            g.0.pop_front();
        }
    }
    unsafe { op::opus_decoder_destroy(dec) };
    drop(stream);
}

// ---------------------------------------------------------------- microphone ---

pub struct Mic {
    pub enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Mic {
    /// `send` gets every encoded packet (from the encoder thread).
    pub fn start(send: impl Fn(Vec<u8>) + Send + 'static) -> Result<Mic, String> {
        let host = cpal::default_host();
        let dev = host.default_input_device().ok_or("no microphone")?;
        let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
        let enabled = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (en, st) = (enabled.clone(), stop.clone());
        let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);
        std::thread::Builder::new()
            .name("audio-in".into())
            .spawn(move || {
                let rate = cfg.sample_rate();
                let channels = cfg.channels() as usize;
                let (tx, rx) = sync_channel::<Vec<f32>>(32);
                let mono = move |interleaved: Vec<f32>| interleaved.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect::<Vec<f32>>();
                let sc: cpal::StreamConfig = cfg.config();
                let stream = match cfg.sample_format() {
                    cpal::SampleFormat::F32 => {
                        let tx = tx.clone();
                        dev.build_input_stream::<f32, _, _>(sc, move |d, _| { let _ = tx.try_send(mono(d.to_vec())); }, |_| {}, None)
                    }
                    cpal::SampleFormat::I16 => {
                        let tx = tx.clone();
                        dev.build_input_stream::<i16, _, _>(sc, move |d, _| { let _ = tx.try_send(mono(d.iter().map(|s| *s as f32 / 32768.0).collect())); }, |_| {}, None)
                    }
                    _ => return,
                };
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
                let mut err = 0;
                let enc = unsafe { op::opus_encoder_create(RATE as i32, 1, op::OPUS_APPLICATION_VOIP as i32, &mut err) };
                if enc.is_null() {
                    let _ = ready_tx.send(Err("opus".into()));
                    return;
                }
                unsafe {
                    op::opus_encoder_ctl(enc, op::OPUS_SET_BITRATE_REQUEST as i32, 32_000i32);
                    op::opus_encoder_ctl(enc, op::OPUS_SET_DTX_REQUEST as i32, 1i32);
                }
                let _ = ready_tx.send(Ok(()));
                let mut rs = Resampler::new(rate, RATE);
                let mut pend: Vec<f32> = Vec::new();
                let mut out = [0u8; 1500];
                while !st.load(Ordering::Relaxed) {
                    let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
                    rs.run(&chunk, &mut pend);
                    while pend.len() >= FRAME {
                        let frame: Vec<f32> = pend.drain(..FRAME).collect();
                        if !en.load(Ordering::Relaxed) {
                            continue;
                        }
                        let n = unsafe { op::opus_encode_float(enc, frame.as_ptr(), FRAME as i32, out.as_mut_ptr(), out.len() as i32) };
                        // DTX: 1-2 byte packets are silence frames; skip most of them.
                        if n > 2 {
                            send(out[..n as usize].to_vec());
                        }
                    }
                }
                unsafe { op::opus_encoder_destroy(enc) };
                drop(stream);
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "microphone timeout".to_string())??;
        Ok(Mic { enabled, stop })
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
