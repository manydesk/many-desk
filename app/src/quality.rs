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
    /// What the path was seen to carry while it was the limit (bits/s, a little under it):
    /// the bitrate never goes above this. Set at once when the link chokes — walking the
    /// level ladder down a 1 Mbit/s link took 40 s of a frozen picture — and raised again
    /// step by step while frames get through.
    cap: Option<f64>,
    cap_good: u32,
    /// throughput of the last ticks (bits/s): the link's size is the most it carried in
    /// that time (one tick alone may have been idle — waiting for a keyframe, a still
    /// screen — and would read far too low)
    carried: std::collections::VecDeque<f64>,
    /// calm ticks before the cap is tried a step higher; doubles when a try chokes the
    /// link again (it sits right at its size), back to the start when one holds
    cap_wait: u32,
    /// ticks since the cap was last raised (a choke soon after means the raise failed)
    since_raise: u32,
    /// the cap's size steps in use, and the cap above which the picture may grow again
    cap_steps: f64,
    cap_grow_at: f64,
}

const CAP_WAIT: u32 = 3;
const CAP_WAIT_MAX: u32 = 30;
/// One try upward: an eighth more.
const CAP_STEP: f64 = 1.125;

/// Ticks (of ~2 s) the capacity estimate looks back over.
const CARRIED_TICKS: usize = 15;

/// The cap is this share of the measured throughput (room for audio, input and retransmits).
const CAP_SHARE: f64 = 0.8;
/// Never below this: a picture every few seconds beats none.
const CAP_FLOOR: f64 = 40e3;
/// A keyframe's size (bytes a pixel of typical screen content) and how long one may take
/// to cross a capped link.
const KEY_BYTES_PER_PIXEL: f64 = 0.1;
const KEY_SECS: f64 = 3.0;
/// Frame rates tried, in order, to fit a capped bitrate before the picture shrinks further.
const CAP_FPS: [f64; 6] = [30.0, 20.0, 15.0, 10.0, 7.0, 5.0];

const CALM: Duration = Duration::from_secs(10);
const CPU_SCALE_MAX: f64 = 2.5;
const RICH_MAX: f64 = 50e6;
/// frame-rate caps a CPU limit walks down before it touches the resolution
const CPU_FPS: [f64; 5] = [60.0, 30.0, 20.0, 15.0, 10.0];
const CALM_MAX: Duration = Duration::from_secs(60);

impl Quality {
    pub fn new() -> Self {
        Quality { mode: Mode::Auto, level: START, view: None, bad: 0, good: 0, calm_until: None, calm: CALM, last_up: None, cpu_fps: CPU_FPS[0], cpu_scale: 1.0, link: None, link_used: None, cpu_bad: 0, cpu_good: 0, prev: None, applied: None, cap: None, cap_good: 0, carried: Default::default(), cap_wait: CAP_WAIT, since_raise: u32::MAX, cap_steps: 0.0, cap_grow_at: 0.0 }
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
        let fit0 = fit.max(1.0);
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
        // A measured cap: fewer frames first, at down to half the usual bits a pixel, then
        // fewer pixels (text stays readable longer than motion stays smooth is worth).
        let (mut scale, mut fps, mut bitrate) = (scale, fps, bitrate);
        if let Some(cap) = self.cap.filter(|c| (bitrate as f64) > *c) {
            let need = |f: f64, sc: f64| (sw / sc) * (sh / sc) * f * q.bpp * 0.5;
            match CAP_FPS.iter().copied().filter(|f| *f <= fps).find(|f| need(*f, scale) <= cap) {
                Some(f) => fps = f,
                None => {
                    fps = CAP_FPS[CAP_FPS.len() - 1].min(fps);
                    scale *= (need(fps, scale) / cap).sqrt().clamp(1.0, 4.0);
                }
            }
            // A keyframe must fit too: about 0.1 byte a pixel for a screen, and it should
            // cross the link in some 3 s — a full-size one takes 16 s at 100 kbit/s.
            let pixels = (sw / scale) * (sh / scale);
            let fit = cap * KEY_SECS / (KEY_BYTES_PER_PIXEL * 8.0);
            if pixels > fit {
                scale *= (pixels / fit).sqrt().min(4.0);
            }
            // Sizes in steps of a quarter: a new size means a new encoder and a keyframe,
            // which a cap that moves a little every tick must not cause.
            let base = fit0 * q.extra * ls * self.cpu_scale;
            let mut steps = ((scale / base).ln() / 1.25f64.ln()).ceil().max(0.0);
            // Smaller at once, larger only with clear room (the cap two steps up): a
            // picture that grows and shrinks around one size pays a keyframe each time.
            if steps < self.cap_steps && cap < self.cap_grow_at {
                steps = self.cap_steps;
            }
            if steps != self.cap_steps {
                self.cap_steps = steps;
                self.cap_grow_at = cap * 1.5;
            }
            scale = base * 1.25f64.powf(steps);
            bitrate = cap as u32;
        } else {
            self.cap_steps = 0.0;
        }
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

    /// Feed the encoder statistics every ~2 s; true if the level changed. `goodput`: what
    /// the viewer says arrived, bits/s (0: not known).
    pub fn tick(&mut self, video: &Value, rtt_ms: f64, cwnd: f64, goodput: f64) -> bool {
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
        let cap_changed = self.tick_cap(reason, goodput);
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
            // with a measured cap the bitrate already fits the link: the ladder stays
            "bandwidth" if self.cap.is_some() => self.good = 0,
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
        cpu_changed || link_changed || cap_changed
    }

    /// The link's cap: down at once to what it carried when it was the limit, back up by
    /// half every second calm tick, gone once it is above anything the presets ask for.
    fn tick_cap(&mut self, reason: &str, goodput: f64) -> bool {
        let before = self.cap;
        if goodput > 0.0 {
            self.carried.push_back(goodput);
            if self.carried.len() > CARRIED_TICKS {
                self.carried.pop_front();
            }
        }
        let capacity = self.carried.iter().copied().fold(0.0, f64::max);
        self.since_raise = self.since_raise.saturating_add(1);
        if reason == "bandwidth" && capacity > 0.0 {
            self.cap_good = 0;
            if self.since_raise <= 3 {
                // the try upward did not hold: wait longer before the next
                self.cap_wait = (self.cap_wait * 2).min(CAP_WAIT_MAX);
                self.since_raise = u32::MAX;
            }
            self.cap = Some((capacity * CAP_SHARE).max(CAP_FLOOR));
        } else if reason == "none" {
            if self.since_raise == 4 {
                self.cap_wait = CAP_WAIT; // it held
            }
            if let Some(c) = self.cap {
                self.cap_good += 1;
                if self.cap_good >= self.cap_wait {
                    self.cap_good = 0;
                    self.since_raise = 0;
                    let up = c * CAP_STEP;
                    self.cap = if up > preset(self.mode).max_br * 2.5 { None } else { Some(up) };
                }
            }
        }
        match (before, self.cap) {
            (Some(a), Some(b)) => (a / b - 1.0).abs() > 0.05,
            (None, None) => false,
            _ => true,
        }
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
            q.tick(&stats(f, if i % 3 == 0 { "bandwidth" } else { "none" }), 30.0, 1e6, 0.0);
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
            q.tick(&stats(f, "cpu"), 1.0, 1e9, 0.0);
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
            q.tick(&stats(f, "none"), 1.0, 1e9, 0.0);
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
            q.tick(&stats(f, "none"), 2.0, 1e6, 0.0);
        }
        let (_, _, fast) = q.params((1548, 970)).unwrap();
        assert!(fast as f64 >= slow as f64 * 2.4, "{slow} → {fast}");
        // a slow path (~1 Mbit/s) gets no more than it can carry
        let mut q = Quality::new();
        q.set_view(1548.0, 970.0);
        let (_, _, base) = q.params((1548, 970)).unwrap();
        for _ in 0..3 {
            f += 60;
            q.tick(&stats(f, "none"), 100.0, 12_500.0, 0.0);
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
            q.tick(&stats(f, "bandwidth"), 30.0, 1.0, 0.0);
            if let Some((scale, _, _)) = q.params((3024, 1964)) {
                scales.push((q.level, scale));
            }
        }
        // the first three steps down keep the full resolution
        assert!(scales.iter().filter(|(l, _)| *l <= 3).all(|(_, s)| *s == 1.0), "{scales:?}");
        assert!(q.level >= 4, "should keep stepping down under a sustained limit: {}", q.level);
    }

    #[test]
    fn a_choked_link_caps_the_bitrate_at_once() {
        let mut q = Quality::new();
        q.set_view(1920.0, 1080.0);
        let (_, _, start) = q.params((1920, 1080)).unwrap();
        assert!(start > 3_000_000);
        q.tick(&stats(30, "none"), 80.0, 1e5, 3e6);
        q.carried.clear();
        // the viewer reports 1 Mbit/s arriving while the encoder asks for 4+
        assert!(q.tick(&stats(60, "bandwidth"), 80.0, 1e5, 1e6));
        let (scale, fps, bitrate) = q.params((1920, 1080)).unwrap();
        assert_eq!(bitrate, 800_000);
        assert!(fps <= 15.0 && scale <= 1.6, "fewer frames before fewer pixels: {fps} fps, scale {scale}");
        // an idle tick (waiting for a keyframe) does not pull the cap down
        q.tick(&stats(61, "bandwidth"), 80.0, 1e5, 2e5);
        assert!(q.params((1920, 1080)).is_none());
        // calm: a try upward after CAP_WAIT ticks, an eighth more
        for _ in 0..CAP_WAIT {
            q.tick(&stats(90, "none"), 80.0, 1e5, 8e5);
        }
        let (_, _, later) = q.params((1920, 1080)).unwrap();
        assert_eq!(later, 900_000);
        // it chokes again: back down, and the next try waits twice as long
        q.tick(&stats(95, "bandwidth"), 80.0, 1e5, 1e6);
        assert_eq!(q.cap_wait, CAP_WAIT * 2);
    }

    #[test]
    fn a_very_slow_link_still_gets_a_picture() {
        let mut q = Quality::new();
        q.set_view(1920.0, 1080.0);
        q.params((1920, 1080));
        q.tick(&stats(30, "none"), 400.0, 1e4, 0.0);
        q.tick(&stats(40, "bandwidth"), 400.0, 1e4, 1e5);
        let (scale, fps, bitrate) = q.params((1920, 1080)).unwrap();
        assert_eq!(bitrate, 80_000);
        assert_eq!(fps, 5.0);
        assert!(scale > 1.5 && scale <= 6.0, "{scale}");
    }

    #[test]
    fn without_reports_the_ladder_works_as_before() {
        let mut q = Quality::new();
        q.set_view(1920.0, 1080.0);
        q.params((1920, 1080));
        for i in 0..6 {
            q.tick(&stats(30 * i, "bandwidth"), 80.0, 1e5, 0.0);
        }
        assert!(q.cap.is_none());
        assert!(q.level > START);
    }
}
