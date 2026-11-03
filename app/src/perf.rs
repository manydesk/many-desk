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

// ---- stall watchdog ----
//
// The UI thread beats once per pass (`beat`); a watchdog thread reports when it stops for
// over half a second ("[stall] UI thread blocked 1.8 s"), always — a frozen window is
// worth a log line. With MANYDESK_STALLDUMP=<dir> it also samples the process during the
// stall (macOS `sample`), so the main thread's stack shows what it was stuck in.

/// Startup steps with the time since the first one (MANYDESK_PERF), once each.
pub fn startup(step: &'static str) {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    static SEEN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let t0 = *T0.get_or_init(std::time::Instant::now);
    if std::env::var_os("MANYDESK_PERF").is_none() || SEEN.lock().map(|mut s| s.contains(&step) || { s.push(step); false }).unwrap_or(true) {
        return;
    }
    eprintln!("[startup] {step} after {} ms", t0.elapsed().as_millis());
}

static BEAT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn beat() {
    BEAT.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// How long since the UI thread last ran the app (ms).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn since_beat_ms() -> u64 {
    now_ms().saturating_sub(BEAT.load(std::sync::atomic::Ordering::Relaxed))
}

/// What the UI thread is doing now, named in stall reports ("os" = outside our code:
/// painting, the swap, window messages).
static PHASE: std::sync::Mutex<&'static str> = std::sync::Mutex::new("start");

pub fn phase(p: &'static str) {
    if let Ok(mut g) = PHASE.lock() {
        *g = p;
    }
}

fn current_phase() -> &'static str {
    PHASE.lock().map(|g| *g).unwrap_or("?")
}

pub fn start_watchdog() {
    #[cfg_attr(not(target_os = "macos"), allow(unused))]
    let dump = std::env::var("MANYDESK_STALLDUMP").ok();
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            // An idle UI thread draws nothing and doesn't beat, so the watchdog wakes it
            // (a repaint request) and only counts it stuck when that goes unanswered. Only
            // after 2 s without a frame: waking it more often kept idle windows repainting
            // ten times a second (battery on phones).
            let mut asked: Option<u64> = None; // oldest wake-up not answered yet
            let mut reported = false;
            #[cfg_attr(not(target_os = "macos"), allow(unused))]
            let mut dumps = 0;
            let mut seen: Vec<&'static str> = Vec::new();
            loop {
                let now = now_ms();
                let last = BEAT.load(std::sync::atomic::Ordering::Relaxed);
                // nothing to watch before the first frame (or in a process whose window
                // never came up, like the one that restarts itself opaque)
                if last == 0 {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                match asked {
                    Some(at) if last >= at => {
                        if reported {
                            eprintln!("[stall] UI thread back after {:.1} s (phases: {})", (last - at) as f64 / 1000.0, seen.join(", "));
                        }
                        asked = None;
                        reported = false;
                        seen.clear();
                    }
                    Some(at) if now - at > 700 && !reported => {
                        reported = true;
                        eprintln!("[stall] UI thread not responding for {:.1} s (in {})", (now - at) as f64 / 1000.0, current_phase());
                        #[cfg(target_os = "macos")]
                        if let (Some(dir), true) = (&dump, dumps < 10) {
                            dumps += 1;
                            let file = format!("{dir}/stall-{}-{at}.txt", std::process::id());
                            let _ = std::process::Command::new("/usr/bin/sample").args([&std::process::id().to_string(), "1", "-file", &file]).output();
                            eprintln!("[stall] sampled into {file}");
                        }
                    }
                    _ => {}
                }
                if reported {
                    let p = current_phase();
                    if !seen.contains(&p) {
                        seen.push(p);
                    }
                }
                if asked.is_none() && now.saturating_sub(last) >= 2000 {
                    asked = Some(now);
                    crate::bus::wake();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .ok();
}
