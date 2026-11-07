//! WASAPI plumbing: mic → capture thread → ring → DSP thread → ring → render thread.
//! Fixed 48 kHz, mono in, stereo out, shared mode. Windows resamples via `autoconvert`.

use crate::dsp::Chain;
use anyhow::{Result, bail};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

const SAMPLE_RATE: usize = 48_000;
/// 10 ms of mono audio.
const FRAME: usize = SAMPLE_RATE / 100;
const RING_FRAMES: usize = 20;
/// If the DSP backlog exceeds this, clocks have drifted: drop old audio to bound latency.
const MAX_BACKLOG_FRAMES: usize = 10;

pub fn list_devices() -> Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    for (label, direction) in [
        ("Recording (input) devices", Direction::Capture),
        ("Playback (output) devices", Direction::Render),
    ] {
        let default = enumerator.get_default_device(direction)?.get_friendlyname()?;
        println!("{label}:");
        for device in &enumerator.get_device_collection(direction)? {
            let name = device?.get_friendlyname()?;
            let mark = if name == default { " (default)" } else { "" };
            println!("  {name}{mark}");
        }
    }
    Ok(())
}

fn find_device(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    needle: Option<&str>,
) -> Result<wasapi::Device> {
    let Some(needle) = needle else {
        return Ok(enumerator.get_default_device(direction)?);
    };
    let needle_lc = needle.to_lowercase();
    let mut seen = Vec::new();
    for device in &enumerator.get_device_collection(direction)? {
        let device = device?;
        let name = device.get_friendlyname()?;
        if name.to_lowercase().contains(&needle_lc) {
            return Ok(device);
        }
        seen.push(name);
    }
    bail!("no device matching {needle:?}; available: {seen:?}")
}

pub fn run(input: Option<String>, output: Option<String>, mut chain: Chain) -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let (cap_prod, cap_cons) = HeapRb::<f32>::new(FRAME * RING_FRAMES).split();
    let (out_prod, out_cons) = HeapRb::<f32>::new(FRAME * 2 * RING_FRAMES).split();

    chain.prepare(SAMPLE_RATE as u32);
    eprintln!(
        "chain latency: {:.1} ms",
        chain.latency_samples() as f32 * 1000.0 / SAMPLE_RATE as f32
    );

    let spawn = |name: &str, f: Box<dyn FnOnce() -> Result<()> + Send>| {
        let running = running.clone();
        let name = name.to_string();
        thread::Builder::new().name(name.clone()).spawn(move || {
            if let Err(err) = f() {
                eprintln!("{name} thread stopped: {err:#}");
            }
            running.store(false, Ordering::Relaxed);
        })
    };

    let r = running.clone();
    let capture = spawn("capture", Box::new(move || capture_loop(input, cap_prod, r)))?;
    let r = running.clone();
    let dsp = spawn("dsp", Box::new(move || dsp_loop(cap_cons, out_prod, chain, r)))?;
    let r = running.clone();
    let render = spawn("render", Box::new(move || render_loop(output, out_cons, r)))?;

    eprintln!("running; press Ctrl-C to stop");
    for handle in [capture, dsp, render] {
        let _ = handle.join();
    }
    bail!("a thread stopped, see messages above")
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
    mut chain: Chain,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let mut mono = [0f32; FRAME];
    let mut stereo = [0f32; FRAME * 2];
    while running.load(Ordering::Relaxed) {
        if input.occupied_len() > MAX_BACKLOG_FRAMES * FRAME {
            input.skip(input.occupied_len() - 2 * FRAME);
        }
        if input.occupied_len() >= FRAME && output.vacant_len() >= FRAME * 2 {
            input.pop_slice(&mut mono);
            chain.process(&mut mono);
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
