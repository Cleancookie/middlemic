//! WASAPI plumbing: mic → capture thread → ring → DSP thread → ring → render thread.
//! Fixed 48 kHz, mono in, stereo out, shared mode. Windows resamples via `autoconvert`.

use crate::dsp;
use crate::settings::Settings;
use anyhow::{Result, anyhow};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

const SAMPLE_RATE: usize = 48_000;
/// 10 ms of mono audio.
const FRAME: usize = SAMPLE_RATE / 100;
const RING_FRAMES: usize = 20;
/// If the DSP backlog exceeds this, clocks have drifted: drop old audio to bound latency.
const MAX_BACKLOG_FRAMES: usize = 10;

pub struct Devices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
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
        })
    })
    .join()
    .map_err(|_| anyhow!("device listing panicked"))?
}

/// Exact name wins, then a case-insensitive substring. `None` is the system default.
fn find_device(
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

fn lower_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: plain Win32 call on the current thread's pseudo-handle.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
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
        let mut spawn = |name: &str, f: Box<dyn FnOnce() -> Result<()> + Send>| -> Result<()> {
            let running = running.clone();
            let error = error.clone();
            let name = name.to_string();
            let handle = thread::Builder::new().name(name.clone()).spawn(move || {
                if let Err(err) = f() {
                    error
                        .lock()
                        .unwrap()
                        .get_or_insert(format!("{name}: {err:#}"));
                    running.store(false, Ordering::Relaxed);
                }
            })?;
            engine.handles.push(handle);
            Ok(())
        };

        let r = running.clone();
        spawn("capture", Box::new(move || capture_loop(input, cap_prod, r)))?;
        let r = running.clone();
        spawn(
            "dsp",
            Box::new(move || dsp_loop(cap_cons, out_prod, settings, r)),
        )?;
        let r = running.clone();
        spawn(
            "render",
            Box::new(move || render_loop(Some(output), out_cons, r)),
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
    running: Arc<AtomicBool>,
) -> Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = find_device(&enumerator, Direction::Capture, needle.as_deref())?;
    eprintln!("input:  {}", device.get_friendlyname()?);

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

    let mut bytes: VecDeque<u8> = VecDeque::with_capacity(FRAME * 16);
    let mut samples: Vec<f32> = Vec::with_capacity(FRAME * 4);
    while running.load(Ordering::Relaxed) {
        capture.read_from_device_to_deque(&mut bytes)?;
        samples.clear();
        while bytes.len() >= 4 {
            let b = [
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
                bytes.pop_front().unwrap(),
            ];
            samples.push(f32::from_le_bytes(b));
        }
        ring.push_slice(&samples);
        event.wait_for_event(1000)?;
    }
    client.stop_stream()?;
    Ok(())
}

fn dsp_loop(
    mut input: impl Consumer<Item = f32>,
    mut output: impl Producer<Item = f32>,
    settings: Arc<Settings>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    lower_priority();
    // Built here, not on the caller's thread: some stages are not `Send`.
    let mut chain = dsp::build_chain(&settings)?;
    chain.prepare(SAMPLE_RATE as u32);
    let meters = &settings.meters;
    let peak = |x: &[f32]| x.iter().fold(0f32, |m, s| m.max(s.abs()));
    let mut load = 0f32;
    let mut mono = [0f32; FRAME];
    let mut stereo = [0f32; FRAME * 2];
    while running.load(Ordering::Relaxed) {
        if input.occupied_len() > MAX_BACKLOG_FRAMES * FRAME {
            input.skip(input.occupied_len() - 2 * FRAME);
        }
        if input.occupied_len() >= FRAME && output.vacant_len() >= FRAME * 2 {
            input.pop_slice(&mut mono);
            meters.record_input(peak(&mono));
            meters.input_scope.write(&mono);
            let started = Instant::now();
            chain.process(&mut mono);
            // Fraction of the 10 ms frame budget, lightly smoothed for the UI.
            load += (started.elapsed().as_secs_f32() / 0.010 - load) * 0.1;
            meters.set_load(load);
            meters.set_latency_ms(chain.latency_samples() as f32 * 1000.0 / SAMPLE_RATE as f32);
            meters.record_output(peak(&mono));
            meters.output_scope.write(&mono);
            for (frame, s) in stereo.chunks_exact_mut(2).zip(mono) {
                frame[0] = s;
                frame[1] = s;
            }
            output.push_slice(&stereo);
        } else {
            thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

fn render_loop(
    needle: Option<String>,
    mut ring: impl Consumer<Item = f32>,
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

    let mut samples: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut write = |frames: usize| -> Result<()> {
        samples.clear();
        samples.resize(frames * 2, 0.0);
        ring.pop_slice(&mut samples); // any shortfall stays silent
        bytes.clear();
        bytes.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
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
