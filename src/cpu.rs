//! Whole-process CPU use, as Task Manager shows it: a share of all logical cores.

use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

const SAMPLE_EVERY: Duration = Duration::from_secs(1);

pub struct CpuMeter {
    cores: f32,
    last_at: Instant,
    last_busy: Duration,
    percent: f32,
}

impl CpuMeter {
    pub fn new() -> Self {
        Self {
            cores: std::thread::available_parallelism().map_or(1, |n| n.get()) as f32,
            last_at: Instant::now(),
            last_busy: busy_time(),
            percent: 0.0,
        }
    }

    /// Percent of the whole machine, refreshed at most once a second.
    pub fn percent(&mut self) -> f32 {
        let wall = self.last_at.elapsed();
        if wall >= SAMPLE_EVERY {
            let busy = busy_time();
            let used = busy.saturating_sub(self.last_busy).as_secs_f32();
            self.percent = 100.0 * used / wall.as_secs_f32() / self.cores;
            self.last_at = Instant::now();
            self.last_busy = busy;
        }
        self.percent
    }
}

/// Kernel plus user time of every thread in this process so far.
fn busy_time() -> Duration {
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the pseudo-handle needs no closing; all four pointers are live locals.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) };
    if ok == 0 {
        return Duration::ZERO;
    }
    let ticks = |t: &FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME counts 100 ns ticks.
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}
