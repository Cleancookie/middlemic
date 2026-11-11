//! `middlemic --profile`: runs a clip through the real live chain as fast as it will go and
//! reports where the 10 ms hop budget is spent. Per-stage times come from the chain's `Timed`
//! wrappers; nothing is measured twice.

use crate::dsp::{self, LiveChain};
use crate::settings::Settings;
use crate::wav;
use anyhow::{Result, bail};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const HOP: usize = (wav::RATE / 100) as usize;
const BUDGET_US: f64 = 10_000.0;
const WARMUP_HOPS: usize = 100;
const NO_COUNTER: &str = "n/a (build with --features profile)";

pub struct Args {
    pub clip: Option<PathBuf>,
    pub hops: usize,
    pub json: bool,
    pub repeat: bool,
}

#[derive(Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Debug)]
pub struct Report {
    pub hops: usize,
    pub stages: Vec<Row>,
    pub total: Row,
    pub misses: usize,
    pub rss_start_kb: Option<u64>,
    pub rss_end_kb: Option<u64>,
    pub allocs_per_hop: Option<f64>,
}

/// Nearest-rank percentile of an ascending slice.
fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (pct / 100.0 * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn row(name: &str, mut micros: Vec<f64>) -> Row {
    micros.sort_by(f64::total_cmp);
    Row { name: name.into(), p50: percentile(&micros, 50.0), p99: percentile(&micros, 99.0), max: percentile(&micros, 100.0) }
}

fn micros(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// Resident memory in kB. Linux only.
fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix("VmRSS:")?.trim().trim_end_matches("kB").trim().parse().ok())
}

#[cfg(feature = "profile")]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static COUNT: AtomicU64 = AtomicU64::new(0);

    struct Counting;

    // SAFETY: every call is forwarded unchanged to `System`.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            COUNT.fetch_add(1, Relaxed);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            COUNT.fetch_add(1, Relaxed);
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            COUNT.fetch_add(1, Relaxed);
            unsafe { System.realloc(p, l, n) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    pub fn count() -> Option<u64> {
        Some(COUNT.load(Relaxed))
    }
}

#[cfg(not(feature = "profile"))]
mod counting {
    pub fn count() -> Option<u64> {
        None
    }
}

/// Deterministic stand-in for noisy speech: a voiced tone with syllable-rate swells over broadband noise.
pub fn synthetic_speech(seconds: usize) -> Vec<f32> {
    let mut seed = 0x2545_F491u32;
    (0..wav::RATE as usize * seconds)
        .map(|i| {
            let t = i as f32 / wav::RATE as f32;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let swell = (0.5 + 0.5 * (t * std::f32::consts::TAU * 3.0).sin()).powi(2);
            let f0 = 120.0 + 20.0 * (t * 1.3).sin();
            let voice: f32 = (1..=8).map(|h| (t * f0 * h as f32 * std::f32::consts::TAU).sin() / h as f32).sum();
            0.25 * swell * voice + 0.03 * noise
        })
        .collect()
}

/// Feeds `clip` (looped as needed) through `live` in hops and times each stage.
pub fn run(settings: &std::sync::Arc<Settings>, live: &mut LiveChain, clip: &[f32], hops: usize) -> Report {
    let names: Vec<(u64, String)> = settings.snapshot().1.iter().map(|i| (i.id, i.name.clone())).collect();
    let order: Vec<u64> = live.stage_times().map(|(id, _)| id).collect();
    let mut per_stage: Vec<Vec<f64>> = order.iter().map(|_| Vec::with_capacity(hops)).collect();
    let mut totals = Vec::with_capacity(hops);
    let mut frame = [0f32; HOP];
    let mut pos = 0;
    let mut hop = |live: &mut LiveChain| {
        for slot in frame.iter_mut() {
            *slot = clip[pos % clip.len()];
            pos += 1;
        }
        let started = Instant::now();
        live.process(&mut frame);
        started.elapsed()
    };
    (0..WARMUP_HOPS).for_each(|_| _ = hop(live));
    let (rss_start_kb, allocs_before) = (rss_kb(), counting::count());
    for _ in 0..hops {
        totals.push(micros(hop(live)));
        for (series, (_, t)) in per_stage.iter_mut().zip(live.stage_times()) {
            series.push(micros(t));
        }
    }
    let allocs_per_hop = counting::count().zip(allocs_before).map(|(after, before)| (after - before) as f64 / hops as f64);
    let rss_end_kb = rss_kb();
    let misses = totals.iter().filter(|&&t| t > BUDGET_US).count();
    let stages = order
        .iter()
        .zip(per_stage)
        .map(|(id, series)| row(names.iter().find(|(n, _)| n == id).map_or("?", |(_, n)| n), series))
        .collect();
    Report { hops, stages, total: row("Total chain", totals), misses, rss_start_kb, rss_end_kb, allocs_per_hop }
}

fn kb(v: Option<u64>) -> String {
    v.map_or_else(|| "n/a".into(), |v| format!("{:.1} MB", v as f64 / 1024.0))
}

pub fn table(r: &Report) -> String {
    let mut out = format!("{:<28} {:>9} {:>9} {:>9} {:>9}\n", "stage", "p50 us", "p99 us", "max us", "% budget");
    for s in r.stages.iter().chain([&r.total]) {
        let _ = writeln!(out, "{:<28} {:>9.1} {:>9.1} {:>9.1} {:>8.2}%", s.name, s.p50, s.p99, s.max, s.p50 / BUDGET_US * 100.0);
    }
    let _ = writeln!(out, "% budget is p50 over the 10 ms hop. Hops measured: {}", r.hops);
    let _ = writeln!(out, "deadline misses (hop > 10 ms): {}", r.misses);
    let _ = writeln!(out, "RSS start / end: {} / {}", kb(r.rss_start_kb), kb(r.rss_end_kb));
    let allocs = r.allocs_per_hop.map_or_else(|| NO_COUNTER.to_string(), |a| format!("{a:.2}"));
    let _ = writeln!(out, "heap allocations per hop: {allocs}");
    out
}

fn json_string(s: &str) -> String {
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

fn json_row(r: &Row) -> String {
    format!("{{\"name\":{},\"p50_us\":{:.1},\"p99_us\":{:.1},\"max_us\":{:.1}}}", json_string(&r.name), r.p50, r.p99, r.max)
}

fn json_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".into(), |v| v.to_string())
}

pub fn json(r: &Report) -> String {
    let stages: Vec<String> = r.stages.iter().map(json_row).collect();
    format!(
        "{{\"hops\":{},\"budget_us\":{},\"stages\":[{}],\"total\":{},\"deadline_misses\":{},\"rss_start_kb\":{},\"rss_end_kb\":{},\"allocs_per_hop\":{}}}",
        r.hops,
        BUDGET_US,
        stages.join(","),
        json_row(&r.total),
        r.misses,
        json_opt(r.rss_start_kb),
        json_opt(r.rss_end_kb),
        json_opt(r.allocs_per_hop.map(|a| format!("{a:.2}"))),
    )
}

pub fn main(args: Args) -> Result<()> {
    if args.hops == 0 {
        bail!("--hops must be at least 1");
    }
    let clip = match &args.clip {
        Some(path) => {
            let (samples, rate) = wav::read_mono(path).map_err(anyhow::Error::msg)?;
            if rate != wav::RATE {
                bail!("{rate} Hz audio is not supported, only {} Hz", wav::RATE);
            }
            if samples.is_empty() {
                bail!("{} has no audio", path.display());
            }
            samples
        }
        None => synthetic_speech(10),
    };
    let settings = Settings::new(dsp::default_sections());
    let mut live = LiveChain::new(settings.clone(), None, wav::RATE)?;
    loop {
        let report = run(&settings, &mut live, &clip, args.hops);
        if args.json {
            println!("{}", json(&report));
        } else {
            print!("{}", table(&report));
        }
        if !args.repeat {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!((percentile(&v, 50.0), percentile(&v, 99.0), percentile(&v, 100.0)), (50.0, 99.0, 100.0));
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    fn sample_report() -> Report {
        Report {
            hops: 10,
            stages: vec![row("EQ \"1\"", vec![3.0, 1.0, 2.0])],
            total: row("Total chain", vec![5.0]),
            misses: 1,
            rss_start_kb: Some(2048),
            rss_end_kb: None,
            allocs_per_hop: None,
        }
    }

    #[test]
    fn json_has_the_documented_shape() {
        assert_eq!(
            json(&sample_report()),
            "{\"hops\":10,\"budget_us\":10000,\"stages\":[{\"name\":\"EQ \\\"1\\\"\",\"p50_us\":2.0,\"p99_us\":3.0,\"max_us\":3.0}],\
             \"total\":{\"name\":\"Total chain\",\"p50_us\":5.0,\"p99_us\":5.0,\"max_us\":5.0},\
             \"deadline_misses\":1,\"rss_start_kb\":2048,\"rss_end_kb\":null,\"allocs_per_hop\":null}"
        );
    }

    #[test]
    fn table_lists_every_stage_and_the_totals() {
        let t = table(&sample_report());
        assert!(t.contains("Total chain") && t.contains("deadline misses (hop > 10 ms): 1"));
        assert!(t.contains("2.0 MB / n/a"));
        #[cfg(not(feature = "profile"))]
        assert!(t.contains(NO_COUNTER));
    }

    #[test]
    fn a_short_synthetic_run_completes_and_loops_a_short_clip() {
        let settings = Settings::new(vec![dsp::new_section("Mic boost").unwrap()]);
        let mut live = LiveChain::new(settings.clone(), None, wav::RATE).unwrap();
        let clip = synthetic_speech(1);
        let short = &clip[..HOP * 3];
        let report = run(&settings, &mut live, short, 150);
        assert_eq!((report.hops, report.stages.len()), (150, 1));
        assert!(report.total.p50 > 0.0 && report.total.p99 >= report.total.p50 && report.total.max >= report.total.p99);
    }
}
