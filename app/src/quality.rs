// Host-side video quality, per viewer. The resolution follows the viewer's stage (never
// more pixels than it can show); "auto" steps size/fps down when the encoder is CPU- or
// network-limited and back up after a calm period. Same rules as the web build.
use std::time::{Duration, Instant};

use serde_json::Value;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Auto,
    Speed,
    Balanced,
    Quality,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "auto" => Mode::Auto,
            "speed" => Mode::Speed,
            "balanced" => Mode::Balanced,
            "quality" => Mode::Quality,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Speed => "speed",
            Mode::Balanced => "balanced",
            Mode::Quality => "quality",
        }
    }
}

struct Preset {
    fps: f64,
    bpp: f64,
    min_br: f64,
    max_br: f64,
    extra: f64,
}

fn preset(m: Mode) -> Preset {
    match m {
        Mode::Auto => Preset { fps: 30.0, bpp: 0.08, min_br: 300e3, max_br: 8e6, extra: 1.0 },
        Mode::Speed => Preset { fps: 20.0, bpp: 0.05, min_br: 200e3, max_br: 1.5e6, extra: 1.5 },
        Mode::Balanced => Preset { fps: 30.0, bpp: 0.08, min_br: 300e3, max_br: 5e6, extra: 1.0 },
        Mode::Quality => Preset { fps: 60.0, bpp: 0.12, min_br: 1e6, max_br: 16e6, extra: 1.0 },
    }
}

// auto steps: extra downscale, fps cap, bitrate factor. Starts at 1. Screen content:
// frame rate and bitrate go first, the resolution only at the last steps — text that
// shrinks becomes unreadable, while fewer frames per second mostly just feel slower.
const LEVELS: [(f64, f64, f64); 6] = [(1.0, 60.0, 1.5), (1.0, 30.0, 1.0), (1.0, 24.0, 0.8), (1.0, 15.0, 0.6), (1.33, 12.0, 0.45), (1.6, 10.0, 0.35)];
const START: usize = 1;

pub struct Quality {
    pub mode: Mode,
    level: usize,
    view: Option<(f64, f64)>,
    bad: u32,
    good: u32,
    calm_until: Option<Instant>,
    /// how long to stay put after stepping down; doubles when a step up didn't hold
    calm: Duration,
    last_up: Option<Instant>,
    /// while the host can't encode fast enough (CPU, e.g. a VM without a GPU encoder): first
    /// a lower frame rate (the picture stays sharp), only then fewer pixels
    cpu_fps: f64,
    cpu_scale: f64,
    /// the path's capacity (BBR window over the round trip), smoothed, bits/s
    link: Option<f64>,
    link_used: Option<f64>,
    cpu_bad: u32,
    cpu_good: u32,
    prev: Option<(u64, Instant)>,
    applied: Option<(f64, f64, u32)>,
}

const CALM: Duration = Duration::from_secs(10);
const CPU_SCALE_MAX: f64 = 2.5;
const RICH_MAX: f64 = 50e6;
/// frame-rate caps a CPU limit walks down before it touches the resolution
const CPU_FPS: [f64; 5] = [60.0, 30.0, 20.0, 15.0, 10.0];
const CALM_MAX: Duration = Duration::from_secs(60);

impl Quality {
    pub fn new() -> Self {
        Quality { mode: Mode::Auto, level: START, view: None, bad: 0, good: 0, calm_until: None, calm: CALM, last_up: None, cpu_fps: CPU_FPS[0], cpu_scale: 1.0, link: None, link_used: None, cpu_bad: 0, cpu_good: 0, prev: None, applied: None }
    }

    pub fn set_mode(&mut self, m: Mode) {
        self.mode = m;
        self.level = START;
    }

    pub fn set_view(&mut self, w: f64, h: f64) {
        if (160.0..=16384.0).contains(&w) && (120.0..=16384.0).contains(&h) {
            self.view = Some((w, h));
        }
    }

    /// (scale, fps, bitrate) for a source of `src` pixels; Some only when it changed.
    pub fn params(&mut self, src: (u32, u32)) -> Option<(f64, f64, u32)> {
        let q = preset(self.mode);
        let auto = self.mode == Mode::Auto;
        let (ls, lfps, lbr) = if auto { LEVELS[self.level] } else { (1.0, q.fps, 1.0) };
        let (sw, sh) = (src.0 as f64, src.1 as f64);
        let fit = self.view.map(|(w, h)| (sw / w).max(sh / h).max(1.0)).unwrap_or(1.0);
        let scale = (fit * q.extra * ls * self.cpu_scale).max(1.0);
        let fps = (q.fps * if auto { 2.0 } else { 1.0 }).min(lfps).min(self.cpu_fps);
        let pixels = (sw / scale) * (sh / scale);
        let base = (q.max_br * lbr).min((pixels * fps * q.bpp * lbr).max(q.min_br));
        // A path with plenty to spare: more bits for the same picture — sharper text, fewer
        // skipped frames — up to 2.5× and never more than half the path (or 50 Mbit/s).
        let bitrate = match self.link {
            Some(link) if link > base * 3.0 => (base * 2.5).min(link * 0.5).min(RICH_MAX).max(base),
            _ => base,
        } as u32;
        self.link_used = self.link;
        let p = ((scale * 1000.0).round() / 1000.0, fps, bitrate);
        if self.applied == Some(p) {
            return None;
        }
        if std::env::var_os("MANYDESK_PERF").is_some() {
            eprintln!("[quality] level {} → {}x{} (scale {:.2}) {} fps {:.1} Mbit/s", self.level, (sw / scale) as u32, (sh / scale) as u32, scale, fps, bitrate as f64 / 1e6);
        }
        self.applied = Some(p);
        Some(p)
    }

    /// Feed the encoder statistics every ~2 s; true if the level changed.
    pub fn tick(&mut self, video: &Value, rtt_ms: f64, cwnd: f64) -> bool {
        if self.mode != Mode::Auto {
            return false;
        }
        let now = Instant::now();
        let frames = video["framesEncoded"].as_u64().unwrap_or(0);
        let prev = self.prev.replace((frames, now));
        if prev.is_none() {
            return false;
        }
        let reason = video["qualityLimitationReason"].as_str().unwrap_or("none");
        // the path's capacity: recompute the bitrate when it moved by a third
        let mut link_changed = false;
        if rtt_ms > 0.0 && cwnd > 0.0 {
            let now_bps = cwnd * 8.0 / (rtt_ms / 1000.0);
            let l = self.link.map(|l| l * 0.7 + now_bps * 0.3).unwrap_or(now_bps);
            self.link = Some(l);
            link_changed = self.link_used.is_none_or(|u| (l / u - 1.0).abs() > 0.33);
        }
        let fps_now = self.applied.map(|a| a.1).unwrap_or(CPU_FPS[0]);
        // CPU: fewer frames first (sharp picture), fewer pixels only at the lowest frame
        // rate; back the other way round (pixels first) once the host keeps up.
        let mut cpu_changed = false;
        if reason == "cpu" {
            self.cpu_good = 0;
            self.cpu_bad += 1;
            if self.cpu_bad >= 2 {
                // each further step needs fresh evidence (the 2 s stats still hold the
                // slow frames from before this step)
                self.cpu_bad = 0;
                let cur = fps_now.min(self.cpu_fps);
                if let Some(&lower) = CPU_FPS.iter().find(|&&f| f < cur) {
                    self.cpu_fps = lower;
                    cpu_changed = true;
                } else if self.cpu_scale < CPU_SCALE_MAX {
                    self.cpu_scale = (self.cpu_scale * 1.25).min(CPU_SCALE_MAX);
                    cpu_changed = true;
                }
            }
        } else {
            self.cpu_bad = 0;
            // grow back only with clear room (most frames well inside their slot), or the
            // next size up is slow again and it pumps between two sizes
            let room = video["fastShare"].as_f64().unwrap_or(1.0) >= 0.8;
            self.cpu_good = if room { self.cpu_good + 1 } else { 0 };
            if self.cpu_good >= 10 {
                if self.cpu_scale > 1.0 {
                    self.cpu_scale = (self.cpu_scale / 1.25).max(1.0);
                    self.cpu_good = 0;
                    cpu_changed = true;
                } else if self.cpu_fps < CPU_FPS[0] {
                    self.cpu_fps = CPU_FPS.iter().rev().copied().find(|&f| f > self.cpu_fps).unwrap_or(CPU_FPS[0]);
                    self.cpu_good = 0;
                    cpu_changed = true;
                }
            }
        }
        // the level ladder answers the network
        // (while the CPU is the limit, it stays put: more frames would only make it worse)
        match reason {
            "bandwidth" => {
                self.bad += 1;
                self.good = 0;
            }
            "cpu" => self.good = 0,
            _ => {
                self.good += 1;
                self.bad = 0;
            }
        }
        let mut level = self.level;
        if self.bad >= 2 && level < LEVELS.len() - 1 {
            level += 1;
            // A step up that didn't hold: wait longer before the next try (no pumping
            // between two levels on a link that sits right in between).
            self.calm = if self.last_up.is_some_and(|t| now.duration_since(t) < Duration::from_secs(30)) { (self.calm * 2).min(CALM_MAX) } else { CALM };
            self.calm_until = Some(now + self.calm);
        } else if self.good >= 3 && level > 0 && self.cpu_scale == 1.0 && self.cpu_fps == CPU_FPS[0] && self.calm_until.is_none_or(|t| now > t) {
            // Step up only with clear spare bandwidth (BBR's window over the RTT): the
            // level above needs up to about twice the bits.
            let avail = if rtt_ms > 0.0 { cwnd * 8.0 / (rtt_ms / 1000.0) } else { f64::MAX };
            let max = self.applied.map(|a| a.2 as f64).unwrap_or(0.0);
            if avail > max * 2.0 {
                level -= 1;
                self.last_up = Some(now);
            }
        }
        if level != self.level {
            self.level = level;
            self.bad = 0;
            self.good = 0;
            return true;
        }
        cpu_changed || link_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stats(frames: u64, reason: &str) -> Value {
        json!({ "framesEncoded": frames, "qualityLimitationReason": reason })
    }

    #[test]
    fn lone_hiccups_keep_the_level() {
        let mut q = Quality::new();
        q.set_view(1512.0, 982.0);
        let start = q.level;
        let mut f = 0;
        // one limited window now and then (Wi-Fi): never two in a row
        for i in 0..20 {
            f += 60;
            q.tick(&stats(f, if i % 3 == 0 { "bandwidth" } else { "none" }), 30.0, 1e6);
        }
        assert!(q.level <= start, "level went from {start} to {}", q.level);
    }

    #[test]
    fn a_slow_encoder_drops_frames_before_pixels() {
        let mut q = Quality::new();
        q.set_view(3024.0, 1898.0);
        let mut f = 0;
        q.params((3024, 1898));
        let mut seen = vec![];
        for _ in 0..24 {
            f += 10;
            q.tick(&stats(f, "cpu"), 1.0, 1e9);
            if let Some(p) = q.params((3024, 1898)) {
                seen.push(p);
            }
        }
        // the frame rate comes down to 10 at full size before the picture shrinks
        let first_shrink = seen.iter().position(|p| p.0 > 1.0).expect("shrinks in the end");
        assert!(seen[..first_shrink].iter().all(|p| p.0 == 1.0));
        assert_eq!(seen[first_shrink - 1].1, 10.0, "{seen:?}");
        // and back once the host keeps up: full size first
        for _ in 0..200 {
            f += 60;
            q.tick(&stats(f, "none"), 1.0, 1e9);
        }
        assert_eq!(q.cpu_scale, 1.0);
        assert_eq!(q.cpu_fps, CPU_FPS[0]);
    }

    #[test]
    fn a_fast_path_gets_more_bits() {
        let mut q = Quality::new();
        q.set_view(1548.0, 970.0);
        let (_, _, slow) = q.params((1548, 970)).unwrap();
        let mut f = 0;
        // LAN: a 1 MB window over 2 ms ≈ 4 Gbit/s
        for _ in 0..3 {
            f += 60;
            q.tick(&stats(f, "none"), 2.0, 1e6);
        }
        let (_, _, fast) = q.params((1548, 970)).unwrap();
        assert!(fast as f64 >= slow as f64 * 2.4, "{slow} → {fast}");
        // a slow path (~1 Mbit/s) gets no more than it can carry
        let mut q = Quality::new();
        q.set_view(1548.0, 970.0);
        let (_, _, base) = q.params((1548, 970)).unwrap();
        for _ in 0..3 {
            f += 60;
            q.tick(&stats(f, "none"), 100.0, 12_500.0);
        }
        assert!(q.params((1548, 970)).is_none_or(|p| p.2 <= base));
    }

    #[test]
    fn sustained_limits_cost_frames_before_pixels() {
        let mut q = Quality::new();
        q.set_view(3024.0, 1964.0);
        let mut f = 0;
        let mut scales = vec![];
        for _ in 0..12 {
            f += 30;
            q.tick(&stats(f, "bandwidth"), 30.0, 1.0);
            if let Some((scale, _, _)) = q.params((3024, 1964)) {
                scales.push((q.level, scale));
            }
        }
        // the first three steps down keep the full resolution
        assert!(scales.iter().filter(|(l, _)| *l <= 3).all(|(_, s)| *s == 1.0), "{scales:?}");
        assert!(q.level >= 4, "should keep stepping down under a sustained limit: {}", q.level);
    }
}
