//! WASAPI plumbing: mic → capture thread → ring → DSP thread → ring → render thread.
//! Fixed 48 kHz, mono in, stereo out, shared mode. Windows resamples via `autoconvert`.

use crate::dsp;
use crate::pcm;
use crate::latency::{BLOCK_MS, CUSHION_MS};
use crate::playout::{Fill, Playout};
use crate::settings::Settings;
use anyhow::{Result, anyhow};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

const SAMPLE_RATE: usize = 48_000;
/// 10 ms of mono audio.
pub(crate) const FRAME: usize = SAMPLE_RATE / 1000 * BLOCK_MS;
pub(crate) const RING_FRAMES: usize = 20;
/// If the DSP backlog exceeds this, clocks have drifted: drop old audio to bound latency.
/// WASAPI reports periods in 100 ns units.
const HNS_PER_MS: f32 = 10_000.0;
/// The meters' load, latency and per-stage stats are published this often (about every 100 ms).
const STATS_EVERY_HOPS: u32 = 10;
const MAX_BACKLOG_FRAMES: usize = 10;
/// The output waits for this much audio (50 ms of stereo) before it starts, and again after
/// running dry, so uneven delivery is absorbed instead of heard as gaps.
const PLAYOUT_CUSHION_SAMPLES: usize = SAMPLE_RATE / 1000 * CUSHION_MS * 2;

pub struct Devices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// What Windows uses for "default recording device".
    pub default_input: Option<String>,
}

/// Runs on its own thread: the UI thread may already own COM in a different apartment mode.
pub fn list_devices() -> Result<Devices> {
    thread::spawn(|| -> Result<Devices> {
        initialize_mta().ok()?;
        let enumerator = DeviceEnumerator::new()?;
        let names = |direction| -> Result<Vec<String>> {
            let mut names = Vec::new();
            for device in &enumerator.get_device_collection(direction)? {
                names.push(device?.get_friendlyname()?);
            }
            Ok(names)
        };
        Ok(Devices {
            inputs: names(Direction::Capture)?,
            outputs: names(Direction::Render)?,
            default_input: enumerator
                .get_default_device(Direction::Capture)
                .and_then(|d| d.get_friendlyname())
                .ok(),
        })
    })
    .join()
    .map_err(|_| anyhow!("device listing panicked"))?
}

/// Exact name wins, then a case-insensitive substring. `None` is the system default.
pub(crate) fn find_device(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    needle: Option<&str>,
) -> Result<wasapi::Device> {
    let Some(needle) = needle else {
        return Ok(enumerator.get_default_device(direction)?);
    };
    let needle_lc = needle.to_lowercase();
    let mut partial = None;
    let mut seen = Vec::new();
    for device in &enumerator.get_device_collection(direction)? {
        let device = device?;
        let name = device.get_friendlyname()?;
        if name == needle {
            return Ok(device);
        }
        if partial.is_none() && name.to_lowercase().contains(&needle_lc) {
            partial = Some(device);
        }
        seen.push(name);
    }
    partial.ok_or_else(|| anyhow!("no device matching {needle:?}; available: {seen:?}"))
}

pub(crate) fn lower_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: plain Win32 call on the current thread's pseudo-handle.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Registers the calling thread with the Windows multimedia scheduler as "Pro Audio", so a game
/// cannot starve it. Best effort: the thread just keeps its normal scheduling if this fails.
fn join_pro_audio() {
    use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut index = 0u32;
    // SAFETY: `task` is NUL-terminated and outlives the call; `index` is a valid out pointer.
    unsafe {
        AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index);
    }
}

/// Capture, DSP and render threads. Stops when dropped or when any thread fails.
pub struct Engine {
    running: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl Engine {
    /// `input: None` is the system default recording device.
    pub fn start(input: Option<String>, output: String, settings: Arc<Settings>) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let error = Arc::new(Mutex::new(None));
        let (cap_prod, cap_cons) = HeapRb::<f32>::new(FRAME * RING_FRAMES).split();
        let (out_prod, out_cons) = HeapRb::<f32>::new(FRAME * 2 * RING_FRAMES).split();

        let mut engine = Self {
            running: running.clone(),
            error: error.clone(),
            handles: Vec::new(),
        };
        let mut spawn = |name: &str, f: Box<dyn FnOnce() -> Result<()> + Send>| -> Result<thread::Thread> {
            let running = running.clone();
            let error = error.clone();
            let name = name.to_string();
            let handle = thread::Builder::new().name(name.clone()).spawn(move || {
                join_pro_audio();
                if let Err(err) = f() {
                    error
                        .lock()
                        .unwrap()
                        .get_or_insert(format!("{name}: {err:#}"));
                    running.store(false, Ordering::Relaxed);
                }
            })?;
            let thread = handle.thread().clone();
            engine.handles.push(handle);
            Ok(thread)
        };

        settings.meters.clear_dropouts();
        let (r, s) = (running.clone(), settings.clone());
        // The capture thread wakes the DSP thread whenever it delivers audio.
        let dsp_thread = spawn("dsp", Box::new(move || dsp_loop(cap_cons, out_prod, s, r)))?;
        let (r, s) = (running.clone(), settings.clone());
        spawn("capture", Box::new(move || capture_loop(input, cap_prod, s, r, dsp_thread)))?;
        let r = running.clone();
        spawn(
            "render",
            Box::new(move || render_loop(Some(output), out_cons, settings, r)),
        )?;
        Ok(engine)
    }

    /// False once stopped, or after any thread failed (see [`Engine::error`]).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn capture_loop(
    needle: Option<String>,
    mut ring: impl Producer<Item = f32>,
    settings: Arc<Settings>,
    running: Arc<AtomicBool>,
    dsp: thread::Thread,
) -> Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = find_device(&enumerator, Direction::Capture, needle.as_deref())?;
    eprintln!("input:  {}", device.get_friendlyname()?);
    let (period, _) = device.get_iaudioclient()?.get_device_period()?;
    settings.meters.set_capture_buffer_ms(period as f32 / HNS_PER_MS);
    let mut push = ring_sink(&mut ring, || settings.meters.record_dropout());
    pump_capture(&device, &running, |samples| {
        push(samples);
        dsp.unpark();
    })
}

/// A sink that pushes into `ring` and calls `on_overflow` when the reader fell behind.
fn ring_sink<'a>(ring: &'a mut impl Producer<Item = f32>, mut on_overflow: impl FnMut() + 'a) -> impl FnMut(&[f32]) + 'a {
    move |samples| {
        if ring.push_slice(samples) < samples.len() {
            on_overflow(); // the processing thread fell behind; audio lost
        }
    }
}

/// Hand `sink` the mono 48 kHz `f32` samples of `device` until `running` clears.
pub(crate) fn pump_capture(
    device: &wasapi::Device,
    running: &AtomicBool,
    mut sink: impl FnMut(&[f32]),
) -> Result<()> {
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, SampleType::Float, SAMPLE_RATE, 1, None);
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        Direction::Capture,
        StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: default_period,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut bytes: Vec<u8> = vec![0; FRAME * 16];
    let mut samples: Vec<f32> = Vec::with_capacity(FRAME * 4);
    while running.load(Ordering::Relaxed) {
        samples.clear();
        // Drain every packet that is waiting, reading each straight into one reused buffer.
        while let Some(frames) = capture.get_next_packet_size()?.filter(|&f| f > 0) {
            let needed = frames as usize * 4;
            if bytes.len() < needed {
                bytes.resize(needed, 0);
            }
            let (read, _) = capture.read_from_device(&mut bytes[..needed])?;
            pcm::append_samples(&bytes[..read as usize * 4], &mut samples);
        }
        sink(&samples);
        event.wait_for_event(1000)?;
    }
    client.stop_stream()?;
    Ok(())
}

/// Listens to several microphones at once, keeping each one's loudest recent sample, so the
/// setup wizard can show which mic hears the user. A mic that cannot be opened just stays at 0.
pub struct MicMonitor {
    running: Arc<AtomicBool>,
    peaks: Vec<(String, Arc<AtomicU32>)>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl MicMonitor {
    /// One listener per entry; `None` is the Windows default mic.
    pub fn start(names: &[Option<String>]) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let mut monitor = Self { running: running.clone(), peaks: Vec::new(), handles: Vec::new() };
        for name in names {
            let peak = Arc::new(AtomicU32::new(0));
            monitor.peaks.push((name.clone().unwrap_or_default(), peak.clone()));
            let (name, running) = (name.clone(), running.clone());
            monitor.handles.push(thread::spawn(move || {
                let listen = || -> Result<()> {
                    initialize_mta().ok()?;
                    let enumerator = DeviceEnumerator::new()?;
                    let device = find_device(&enumerator, Direction::Capture, name.as_deref())?;
                    // Non-negative floats order like their bits, so a max on the bits is a max on the value.
                    pump_capture(&device, &running, |s| {
                        let loudest = s.iter().fold(0f32, |m, x| m.max(x.abs()));
                        peak.fetch_max(loudest.to_bits(), Ordering::Relaxed);
                    })
                };
                let _ = listen();
            }));
        }
        monitor
    }

    /// Each mic's loudest sample since the last call, as a fraction of full scale.
    pub fn take_peaks(&self) -> Vec<(String, f32)> {
        self.peaks.iter().map(|(n, p)| (n.clone(), f32::from_bits(p.swap(0, Ordering::Relaxed)))).collect()
    }
}

impl Drop for MicMonitor {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.handles.drain(..).for_each(|h| {
            let _ = h.join();
        });
    }
}

/// Denormal floats make filters and decays slow on x86; flush them to zero on this thread.
fn flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads and writes only this thread's MXCSR, setting FTZ (bit 15) and DAZ (bit 6).
    unsafe {
        let mut csr = 0u32;
        core::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040;
        core::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly));
    }
}

fn dsp_loop(
    mut input: impl Consumer<Item = f32>,
    mut output: impl Producer<Item = f32>,
    settings: Arc<Settings>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    flush_denormals();
    let mut chain = dsp::LiveChain::new(settings.clone(), SAMPLE_RATE as u32)?;
    let meters = &settings.meters;
    let peak = |x: &[f32]| x.iter().fold(0f32, |m, s| m.max(s.abs()));
    let mut load = 0f32;
    let mut hops = 0u32;
    let mut mono = [0f32; FRAME];
    let mut stereo = [0f32; FRAME * 2];
    while running.load(Ordering::Relaxed) {
        if input.occupied_len() > MAX_BACKLOG_FRAMES * FRAME {
            input.skip(input.occupied_len() - 2 * FRAME);
            meters.record_dropout(); // clocks drifted apart; old audio dropped to catch up
        }
        if input.occupied_len() >= FRAME && output.vacant_len() >= FRAME * 2 {
            input.pop_slice(&mut mono);
            meters.record_input(peak(&mono));
            meters.input_scope.write(&mono);
            let started = Instant::now();
            chain.process(&mut mono);
            // Fraction of the 10 ms frame budget, lightly smoothed for the UI.
            load += (started.elapsed().as_secs_f32() / 0.010 - load) * 0.1;
            hops += 1;
            if hops % STATS_EVERY_HOPS == 0 {
                meters.set_load(load);
                let to_ms = 1000.0 / SAMPLE_RATE as f32;
                meters.set_latency_ms(chain.latency_samples() as f32 * to_ms);
                settings.set_stage_stats(chain.stage_stats().map(|(id, n, load)| (id, n as f32 * to_ms, load)));
            }
            meters.record_output(peak(&mono));
            meters.output_scope.write(&mono);
            for (frame, s) in stereo.chunks_exact_mut(2).zip(mono) {
                frame[0] = s;
                frame[1] = s;
            }
            output.push_slice(&stereo);
        } else if input.occupied_len() >= FRAME {
            thread::sleep(Duration::from_millis(2)); // the render side is behind; it frees room soon
        } else {
            // The capture thread unparks us with the next block; the timeout only bounds a stall.
            thread::park_timeout(Duration::from_millis(10));
        }
    }
    Ok(())
}

pub(crate) fn render_loop(
    needle: Option<String>,
    mut ring: impl Consumer<Item = f32>,
    settings: Arc<Settings>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = find_device(&enumerator, Direction::Render, needle.as_deref())?;
    eprintln!("output: {}", device.get_friendlyname()?);

    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, SampleType::Float, SAMPLE_RATE, 2, None);
    let (default_period, _) = client.get_device_period()?;
    client.initialize_client(
        &format,
        Direction::Render,
        StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: default_period,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;
    settings.meters.set_render_buffer_ms(client.get_buffer_size()? as f32 * 1000.0 / SAMPLE_RATE as f32);

    let mut samples: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut playout = Playout::new(PLAYOUT_CUSHION_SAMPLES);
    let mut write = |frames: usize| -> Result<()> {
        samples.clear();
        samples.resize(frames * 2, 0.0);
        if playout.fill(&mut ring, &mut samples) == Fill::RanDry {
            settings.meters.record_dropout();
        }
        pcm::write_bytes(&samples, &mut bytes);
        render.write_to_device(&bytes, None)?;
        Ok(())
    };

    write(client.get_available_space_in_frames()? as usize)?;
    client.start_stream()?;
    while running.load(Ordering::Relaxed) {
        event.wait_for_event(1000)?;
        write(client.get_available_space_in_frames()? as usize)?;
    }
    client.stop_stream()?;
    Ok(())
}
