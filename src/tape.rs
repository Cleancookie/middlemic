//! Recording the raw mic to a file and playing a file back through the rack.
//!
//! Playback is its own chain on the same rack settings as the live engine, so every knob you
//! move is heard on the recording straight away.

use crate::audio::{FRAME, RING_FRAMES, find_device, lower_priority, pump_capture, render_loop};
use crate::dsp::LiveChain;
use crate::settings::Settings;
use crate::setup::Progress;
use crate::wav;
use anyhow::{Result, bail};
use ringbuf::{
    HeapRb,
    traits::{Observer, Producer, Split},
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use wasapi::{DeviceEnumerator, Direction, initialize_mta};

type SharedError = Arc<Mutex<Option<String>>>;

/// Writes the mic to `path` until stopped or dropped.
pub struct Recorder {
    running: Arc<AtomicBool>,
    samples: Arc<AtomicU64>,
    error: SharedError,
    handle: Option<JoinHandle<()>>,
}

impl Recorder {
    /// `input: None` is the Windows default recording device.
    pub fn start(input: Option<String>, path: PathBuf) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut writer = wav::Writer::create(&path)?;
        let running = Arc::new(AtomicBool::new(true));
        let samples = Arc::new(AtomicU64::new(0));
        let error: SharedError = Arc::default();
        let (r, n, e) = (running.clone(), samples.clone(), error.clone());
        let handle = thread::Builder::new().name("record".into()).spawn(move || {
            let result = (|| -> Result<()> {
                initialize_mta().ok()?;
                let enumerator = DeviceEnumerator::new()?;
                let device = find_device(&enumerator, Direction::Capture, input.as_deref())?;
                let mut failed = None;
                pump_capture(
                    &device,
                    &r,
                    |chunk| {
                        if failed.is_none() {
                            match writer.write(chunk) {
                                Ok(()) => drop(n.fetch_add(chunk.len() as u64, Ordering::Relaxed)),
                                Err(err) => failed = Some(err),
                            }
                        }
                    },
                    false,
                )?;
                writer.finish()?;
                failed.map_or(Ok(()), |err| Err(err.into()))
            })();
            if let Err(err) = result {
                *e.lock().unwrap() = Some(format!("{err:#}"));
            }
            r.store(false, Ordering::Relaxed);
        })?;
        Ok(Self { running, samples, error, handle: Some(handle) })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn seconds(&self) -> f32 {
        self.samples.load(Ordering::Relaxed) as f32 / wav::RATE as f32
    }

    /// Stops, finishes the file and says what went wrong, if anything.
    pub fn finish(mut self) -> Result<(), String> {
        self.join();
        match self.error.lock().unwrap().take() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    fn join(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.join();
    }
}

/// Plays `path` through the rack to an output device until it ends, or loops until stopped.
pub struct Player {
    running: Arc<AtomicBool>,
    position: Arc<AtomicUsize>,
    len: usize,
    error: SharedError,
    looping: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    pub path: PathBuf,
}

impl Player {
    /// `output: None` is the Windows default playback device.
    pub fn start(path: PathBuf, output: Option<String>, settings: Arc<Settings>, looping: bool) -> Result<Self> {
        let (samples, rate) = wav::read_mono(&path).map_err(anyhow::Error::msg)?;
        if rate != wav::RATE {
            bail!("{rate} Hz audio is not supported, only {} Hz", wav::RATE);
        }
        let len = samples.len();
        let looping = Arc::new(AtomicBool::new(looping));
        let running = Arc::new(AtomicBool::new(true));
        let position = Arc::new(AtomicUsize::new(0));
        let error: SharedError = Arc::default();
        let (out_prod, out_cons) = HeapRb::<f32>::new(FRAME * 2 * RING_FRAMES).split();

        let mut handles = Vec::new();
        let mut spawn = |name: &str, job: Box<dyn FnOnce() -> Result<()> + Send>| -> Result<()> {
            let (running, error, label) = (running.clone(), error.clone(), name.to_string());
            handles.push(thread::Builder::new().name(label.clone()).spawn(move || {
                if let Err(err) = job() {
                    error.lock().unwrap().get_or_insert(format!("{label}: {err:#}"));
                }
                running.store(false, Ordering::Relaxed);
            })?);
            Ok(())
        };
        let (r, p, s, l) = (running.clone(), position.clone(), settings.clone(), looping.clone());
        spawn("playback-dsp", Box::new(move || feed(samples, out_prod, s, r, p, l)))?;
        let r = running.clone();
        spawn("playback-render", Box::new(move || render_loop(output, out_cons, settings, r)))?;
        Ok(Self { running, position, len, error, looping, handles, path })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// How far through, 0..1.
    pub fn fraction(&self) -> f32 {
        if self.len == 0 { 0.0 } else { (self.position.load(Ordering::Relaxed) as f32 / self.len as f32).min(1.0) }
    }

    pub fn seek(&self, fraction: f32) {
        let frame = (fraction.clamp(0.0, 1.0) * self.len as f32) as usize / FRAME * FRAME;
        self.position.store(frame, Ordering::Relaxed);
    }

    pub fn set_looping(&self, on: bool) {
        self.looping.store(on, Ordering::Relaxed);
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    fn join(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.handles.drain(..).for_each(|h| {
            let _ = h.join();
        });
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.join();
    }
}

/// Pushes the file through a live chain as fast as the output ring has room, 10 ms at a time.
fn feed(
    samples: Vec<f32>,
    mut out: impl Producer<Item = f32> + Observer,
    settings: Arc<Settings>,
    running: Arc<AtomicBool>,
    position: Arc<AtomicUsize>,
    looping: Arc<AtomicBool>,
) -> Result<()> {
    lower_priority();
    let mut chain = LiveChain::new(settings.clone(), None, wav::RATE)?;
    let meters = &settings.meters;
    let to_ms = 1000.0 / wav::RATE as f32;
    let (mut mono, mut stereo) = ([0f32; FRAME], [0f32; FRAME * 2]);
    let mut frames = 0u32;
    while running.load(Ordering::Relaxed) {
        if out.vacant_len() < FRAME * 2 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let at = position.load(Ordering::Relaxed);
        // Past the end by the chain's delay, so its tail is heard too.
        if at >= samples.len() + chain.latency_samples() + FRAME {
            if looping.load(Ordering::Relaxed) {
                position.store(0, Ordering::Relaxed);
                continue;
            }
            break;
        }
        for (i, slot) in mono.iter_mut().enumerate() {
            *slot = samples.get(at + i).copied().unwrap_or(0.0);
        }
        meters.input_scope.write(&mono);
        chain.process(&mut mono);
        meters.output_scope.write(&mono);
        meters.record_output(mono.iter().fold(0f32, |m, s| m.max(s.abs())));
        for (frame, s) in stereo.chunks_exact_mut(2).zip(mono) {
            frame[0] = s;
            frame[1] = s;
        }
        out.push_slice(&stereo);
        // A seek that landed meanwhile wins over moving on.
        let _ = position.compare_exchange(at, at + FRAME, Ordering::Relaxed, Ordering::Relaxed);
        frames += 1;
        if frames % 10 == 0 {
            settings.set_stage_latencies(chain.stage_latencies().map(|(id, n)| (id, n as f32 * to_ms)));
        }
    }
    // Let what is already queued play out before the render thread is told to stop.
    for _ in 0..100 {
        if !running.load(Ordering::Relaxed) || out.occupied_len() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Runs `source` through the rack as it is now and saves `target`, on a background thread.
pub fn spawn_render(settings: Arc<Settings>, source: PathBuf, target: PathBuf) -> Arc<Mutex<Progress>> {
    let progress = Arc::new(Mutex::new(Progress { message: "Rendering…".into(), outcome: None }));
    let shared = progress.clone();
    thread::spawn(move || {
        let say = |fraction: f32| shared.lock().unwrap().message = format!("Rendering {:.0}%", fraction * 100.0);
        let outcome = crate::render::render_file(&settings, &source, &target, say).map(|()| display_name(&target));
        shared.lock().unwrap().outcome = Some(outcome);
    });
    progress
}

fn display_name(path: &Path) -> String {
    path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}
