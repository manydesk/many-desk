// MANYDESK_PERF=1: per-stage timings of the video path, printed every 2 s as
// "[perf] stage: n/s avg ms max ms". Costs nothing when the variable is unset.
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Stats {
    since: Instant,
    stages: BTreeMap<&'static str, (u32, Duration, Duration)>,
    ticks: BTreeMap<&'static str, u32>,
}

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MANYDESK_PERF").is_some())
}

fn stats() -> &'static Mutex<Stats> {
    static S: OnceLock<Mutex<Stats>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Stats { since: Instant::now(), stages: BTreeMap::new(), ticks: BTreeMap::new() }))
}

pub fn add(stage: &'static str, d: Duration) {
    if !on() {
        return;
    }
    let mut s = stats().lock().unwrap();
    let e = s.stages.entry(stage).or_default();
    e.0 += 1;
    e.1 += d;
    e.2 = e.2.max(d);
    flush(&mut s);
}

pub fn tick(name: &'static str) {
    if !on() {
        return;
    }
    let mut s = stats().lock().unwrap();
    *s.ticks.entry(name).or_default() += 1;
    flush(&mut s);
}

fn flush(s: &mut Stats) {
    let el = s.since.elapsed();
    if el < Duration::from_secs(2) {
        return;
    }
    let secs = el.as_secs_f64();
    let mut line = String::from("[perf]");
    for (k, (n, total, max)) in &s.stages {
        line += &format!(" {k}: {:.1}/s {:.2}ms (max {:.1})", *n as f64 / secs, total.as_secs_f64() * 1000.0 / (*n).max(1) as f64, max.as_secs_f64() * 1000.0);
    }
    for (k, n) in &s.ticks {
        line += &format!(" {k}: {:.1}/s", *n as f64 / secs);
    }
    eprintln!("{line}");
    s.since = Instant::now();
    s.stages.clear();
    s.ticks.clear();
}

/// Every 30th frame: "[lat] side seq ms" with the wall clock, to line up host and viewer
/// logs when both run on one machine.
pub fn stamp(side: &str, seq: u32) {
    if !on() || seq % 30 != 0 {
        return;
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
    eprintln!("[lat] {side} {seq} {ms:.1}");
}
