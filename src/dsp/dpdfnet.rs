use super::Processor;
use super::cap::ReductionCap;
use crate::settings::{Param, Section};
use crate::setup::Fetch;
use anyhow::{Context, Result, anyhow, ensure};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ort::environment::Environment;
use ort::ep;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{IoBinding, Session};
use ort::value::Tensor;

const SEARCH_EVERY: Duration = Duration::from_secs(1);

const SAMPLE_RATE: u32 = 48_000;
const HOP: usize = 480;
const N_FFT: usize = 2 * HOP;
const BINS: usize = N_FFT / 2 + 1;
/// Interleaved real and imaginary parts, the shape the model takes.
const SPEC_LEN: usize = BINS * 2;
/// Measured by cross-correlating output against input (`compare_denoisers` in `offline.rs`):
/// 2400 samples, 50 ms. The model sees 40 ms of context beyond the framing.
const LATENCY_SAMPLES: usize = 5 * HOP;
const DEFAULT_MAX_REDUCTION_DB: f32 = 18.0;

/// Which DPDFNet model a stage runs. The files are Ceva's, Apache-2.0, from
/// huggingface.co/Ceva-IP/DPDFNet (`onnx/<file>`). Only the light model: the quality one
/// (`dpdfnet8_48khz_hr`) needs more than a whole core.
#[derive(Clone, Copy)]
pub struct Variant {
    pub name: &'static str,
    pub file: &'static str,
    /// A fixed revision of the Hugging Face repo, so the checksum below stays true.
    pub url: &'static str,
    pub sha256: &'static str,
    pub megabytes: u32,
}

pub const LIGHT: Variant = Variant {
    name: "Noise removal (DPDFNet)",
    file: "dpdfnet2_48khz_hr.onnx",
    url: "https://huggingface.co/Ceva-IP/DPDFNet/resolve/dd6818d00f50c836fed43a6243ebe49116de5964/onnx/dpdfnet2_48khz_hr.onnx",
    sha256: "7f0575a5cec0ba4ffd8f8bd657e06d007e4ccdd955d76faab922b9d3291dc14b",
    megabytes: 10,
};

/// Where the app's own download lands, and the last place looked at.
pub fn download_dir() -> PathBuf {
    crate::settings::settings_path()
        .parent()
        .map_or_else(|| PathBuf::from("models"), |dir| dir.join("models"))
}

/// Both the model file and the ONNX Runtime library are in place.
pub fn model_installed(variant: &Variant) -> bool {
    missing_downloads(variant).is_empty()
}

/// The default noise removal: it kept speech best of the three on Alex's recording.
pub fn section(variant: Variant) -> Section {
    Section::new(
        variant.name,
        vec![Param::new("Max reduction", "dB", 0.0, 100.0, DEFAULT_MAX_REDUCTION_DB).help(
            "The most noise this can turn down, in dB (decibels: about 6 dB is half the loudness). \
             Start at 18. Raise it for a noisy room; lower it if soft word endings sound chopped. 100 means no limit.",
        )],
    )
    .help(
        "AI noise removal (DPDFNet) that removes fans, hum, keyboard and room noise while keeping your voice clear. \
         Needs a one-time model download, adds about 50 ms of delay and uses about 15% of a CPU core.",
    )
    .unfolded()
}

/// Where model files are looked for, best first.
fn model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("MIDDLEMIC_MODELS") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(exe) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe.join("models"));
        dirs.push(exe);
    }
    dirs.push(download_dir());
    dirs
}

fn find_model(file: &str) -> Result<PathBuf> {
    let dirs = model_dirs();
    dirs.iter().map(|d| d.join(file)).find(|p| p.is_file()).ok_or_else(|| {
        let searched: Vec<_> = dirs.iter().map(|d| d.display().to_string()).collect();
        anyhow!(
            "Model file {file} not found. Use Download below, or put it in one of: {}",
            searched.join("  |  ")
        )
    })
}

/// ONNX Runtime library, loaded at run time from `onnxruntime.dll` (Microsoft, MIT). The
/// download is the official release zip, from which only the DLL is kept.
pub const RUNTIME: Fetch = Fetch {
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-win-x64-1.31.1.zip",
    file: "onnxruntime.dll",
    sha256: "5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c",
    member: Some("onnxruntime-win-x64-1.31.1/lib/onnxruntime.dll"),
    megabytes: 83,
};

fn find_runtime() -> Result<PathBuf> {
    match std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from) {
        Some(path) if path.is_file() => Ok(path),
        _ => find_model(RUNTIME.file),
    }
}

/// Everything the stage still needs from the network, runtime first.
pub fn missing_downloads(variant: &Variant) -> Vec<Fetch> {
    let model = Fetch { url: variant.url, file: variant.file, sha256: variant.sha256, member: None, megabytes: variant.megabytes };
    let mut missing = Vec::new();
    if find_runtime().is_err() {
        missing.push(RUNTIME);
    }
    if find_model(variant.file).is_err() {
        missing.push(model);
    }
    missing
}

/// One ONNX Runtime environment per process; it loads the library on first use.
fn environment() -> Result<Environment> {
    static ENV: Mutex<Option<Environment>> = Mutex::new(None);
    let mut slot = ENV.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(env) = slot.as_ref() {
        return Ok(env.clone());
    }
    let dll = find_runtime()?;
    let env = ort::init_from(&dll)
        .map_err(|e| anyhow!("loading {}: {e}", dll.display()))?
        .with_name("middlemic")
        .with_telemetry(false)
        .build()
        .map_err(|e| anyhow!("starting ONNX Runtime: {e}"))?;
    *slot = Some(env.clone());
    Ok(env)
}

fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{e}")
}

/// The model and its recurrent state. One call per hop, with every buffer made up front:
/// the tensors are bound to the session once and rewritten in place.
struct Net {
    session: Session,
    binding: IoBinding,
    spec_in: Tensor<f32>,
    state_in: Tensor<f32>,
    spec_out: Tensor<f32>,
    state_out: Tensor<f32>,
}

impl Net {
    fn load(path: &Path) -> Result<Self> {
        let env = environment()?;
        let session = Session::builder(&env)
            .map_err(ort_err)?
            // One thread and no spinning: waiting workers would burn a core beside a game.
            .with_intra_threads(1)
            .and_then(|b| b.with_inter_threads(1))
            .and_then(|b| b.with_intra_op_spinning(false))
            .and_then(|b| b.with_inter_op_spinning(false))
            .and_then(|b| b.with_parallel_execution(false))
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::All))
            // Fixed shapes: a memory pattern and an arena would only keep memory around.
            .and_then(|b| b.with_memory_pattern(false))
            .and_then(|b| b.with_execution_providers([ep::CPU::default().with_arena_allocator(false).build()]))
            .map_err(ort_err)?
            .commit_from_file(path)
            .with_context(|| format!("reading {}", path.display()))?;

        let meta = session.metadata().map_err(ort_err)?;
        let custom = |key: &str| meta.custom(key);
        let floats = |key: &str| -> Vec<f32> {
            custom(key)
                .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
                .unwrap_or_default()
        };
        let size = |key: &str, default: usize| {
            custom(key).and_then(|s| s.trim().parse().ok()).unwrap_or(default)
        };
        let state_size = size("state_size", 0);
        ensure!(state_size > 0, "{} has no state_size metadata", path.display());
        let (erb_len, spec_len) = (size("erb_norm_state_size", 481), size("spec_norm_state_size", 96));
        let (erb, spec) = (floats("erb_norm_init"), floats("spec_norm_init"));
        drop(meta);

        let names = |outlets: &[ort::value::Outlet]| outlets.iter().map(|o| o.name().to_string()).collect::<Vec<_>>();
        let (inputs, outputs) = (names(session.inputs()), names(session.outputs()));
        ensure!(inputs.len() == 2 && outputs.len() == 2, "model has {} inputs and {} outputs, expected 2 and 2", inputs.len(), outputs.len());

        let allocator = session.allocator();
        let spec_in = Tensor::<f32>::new(&allocator, [1, 1, BINS, 2]).map_err(ort_err)?;
        let mut state_in = Tensor::<f32>::new(&allocator, [state_size]).map_err(ort_err)?;
        let mut spec_out = Tensor::<f32>::new(&allocator, [1, 1, BINS, 2]).map_err(ort_err)?;
        let mut state_out = Tensor::<f32>::new(&allocator, [state_size]).map_err(ort_err)?;

        let init = state_in.extract_tensor_mut().1;
        init.fill(0.0);
        if erb.len() == erb_len && erb_len <= state_size {
            init[..erb_len].copy_from_slice(&erb);
        }
        if spec.len() == spec_len && erb_len + spec_len <= state_size {
            init[erb_len..erb_len + spec_len].copy_from_slice(&spec);
        }

        let mut binding = session.create_binding().map_err(ort_err)?;
        binding.bind_input(&inputs[0], &spec_in).map_err(ort_err)?;
        binding.bind_input(&inputs[1], &state_in).map_err(ort_err)?;
        binding.bind_output(&outputs[0], &mut spec_out).map_err(ort_err)?;
        binding.bind_output(&outputs[1], &mut state_out).map_err(ort_err)?;
        Ok(Self { session, binding, spec_in, state_in, spec_out, state_out })
    }

    fn run(&mut self, spec: &[f32; SPEC_LEN], spec_e: &mut [f32; SPEC_LEN]) -> Result<()> {
        self.spec_in.extract_tensor_mut().1.copy_from_slice(spec);
        self.session.run_binding(&self.binding).map_err(ort_err)?;
        let enhanced = self.spec_out.extract_tensor().1;
        let next = self.state_out.extract_tensor().1;
        let state = self.state_in.extract_tensor_mut().1;
        ensure!(
            enhanced.len() == SPEC_LEN && next.len() == state.len(),
            "model outputs are {} and {} values, expected {SPEC_LEN} and {}",
            enhanced.len(),
            next.len(),
            state.len()
        );
        spec_e.copy_from_slice(enhanced);
        state.copy_from_slice(next);
        Ok(())
    }
}

/// Causal STFT with a Vorbis window and 50% overlap: 960-sample frames every 480 samples.
struct Stft {
    window: Vec<f32>,
    ring: Vec<f32>,
    ola: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    time: Vec<f32>,
    bins: Vec<Complex32>,
}

impl Stft {
    fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let half = N_FFT as f32 / 2.0;
        let window = (0..N_FFT)
            .map(|n| {
                let s = (0.5 * std::f32::consts::PI * (n as f32 + 0.5) / half).sin();
                (0.5 * std::f32::consts::PI * s * s).sin()
            })
            .collect();
        Self {
            window,
            ring: vec![0.0; N_FFT],
            ola: vec![0.0; N_FFT],
            forward: planner.plan_fft_forward(N_FFT),
            inverse: planner.plan_fft_inverse(N_FFT),
            time: vec![0.0; N_FFT],
            bins: vec![Complex32::new(0.0, 0.0); BINS],
        }
    }

    fn analyse(&mut self, hop: &[f32], spec: &mut [f32; SPEC_LEN]) {
        self.ring.copy_within(HOP.., 0);
        self.ring[N_FFT - HOP..].copy_from_slice(hop);
        for ((t, r), w) in self.time.iter_mut().zip(&self.ring).zip(&self.window) {
            *t = r * w;
        }
        self.forward.process(&mut self.time, &mut self.bins).expect("fixed sizes");
        for (pair, bin) in spec.chunks_exact_mut(2).zip(&self.bins) {
            pair[0] = bin.re;
            pair[1] = bin.im;
        }
    }

    fn synthesise(&mut self, spec: &[f32; SPEC_LEN], hop: &mut [f32]) {
        for (bin, pair) in self.bins.iter_mut().zip(spec.chunks_exact(2)) {
            *bin = Complex32::new(pair[0], pair[1]);
        }
        // A real signal's DC and Nyquist bins have no imaginary part.
        self.bins[0].im = 0.0;
        self.bins[BINS - 1].im = 0.0;
        self.inverse.process(&mut self.bins, &mut self.time).expect("fixed sizes");
        let norm = 1.0 / N_FFT as f32;
        self.ola.copy_within(HOP.., 0);
        self.ola[N_FFT - HOP..].fill(0.0);
        for ((o, t), w) in self.ola.iter_mut().zip(&self.time).zip(&self.window) {
            *o += t * norm * w;
        }
        hop.copy_from_slice(&self.ola[..HOP]);
    }
}

/// DPDFNet noise suppression (Ceva, DeepFilterNet family). 48 kHz mono, in multiples of 480
/// samples. The model file is loaded the first time the stage runs; if it is missing the stage
/// says so in its row and passes audio through.
pub struct DpdfNet {
    section: Arc<Section>,
    variant: Variant,
    net: Option<Net>,
    /// The file is there but would not load: do not try again.
    broken: bool,
    /// A missing file is looked for again once a second, so a download is picked up live.
    last_search: Option<Instant>,
    stft: Stft,
    cap: ReductionCap,
    cap_db: f32,
    spec: [f32; SPEC_LEN],
    spec_e: [f32; SPEC_LEN],
    original: [f32; HOP],
}

impl DpdfNet {
    pub fn new(section: Arc<Section>, variant: Variant) -> Self {
        let mut cap = ReductionCap::new(LATENCY_SAMPLES);
        let cap_db = section.get(0);
        cap.set_db(cap_db);
        Self {
            section,
            variant,
            net: None,
            broken: false,
            last_search: None,
            stft: Stft::new(),
            cap,
            cap_db,
            spec: [0.0; SPEC_LEN],
            spec_e: [0.0; SPEC_LEN],
            original: [0.0; HOP],
        }
    }

    fn load(&mut self) {
        self.last_search = Some(Instant::now());
        let path = match find_model(self.variant.file) {
            Ok(path) => path,
            Err(missing) => return self.section.set_status(format!("{missing:#}")),
        };
        // ort panics when the library it loads is too old for it; report that like any other failure.
        let loaded = std::panic::catch_unwind(|| Net::load(&path))
            .unwrap_or_else(|_| Err(anyhow!("onnxruntime.dll could not be used: is it an old version? Delete it to download the right one")));
        match loaded {
            Ok(net) => {
                self.net = Some(net);
                self.section.set_status("");
            }
            Err(err) => {
                self.broken = true;
                self.section.set_status(format!("{err:#}"));
            }
        }
    }

    fn should_look_for_model(&self) -> bool {
        self.net.is_none()
            && !self.broken
            && self.last_search.is_none_or(|at| at.elapsed() >= SEARCH_EVERY)
    }
}

impl Processor for DpdfNet {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    /// Loads the model here, so a stage built off the audio thread arrives ready.
    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "DPDFNet needs 48 kHz");
        if self.should_look_for_model() {
            self.load();
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % HOP, 0);
        if self.should_look_for_model() {
            self.load();
        }
        let Some(net) = self.net.as_mut() else {
            return;
        };
        let db = self.section.get(0);
        if db != self.cap_db {
            self.cap_db = db;
            self.cap.set_db(db);
        }
        for chunk in samples.chunks_exact_mut(HOP) {
            self.original.copy_from_slice(chunk);
            self.stft.analyse(chunk, &mut self.spec);
            if net.run(&self.spec, &mut self.spec_e).is_err() {
                // Keep the framing in step: a silent frame, then carry on with the old state.
                self.spec_e.fill(0.0);
            }
            self.stft.synthesise(&self.spec_e, chunk);
            self.cap.apply(&self.original, chunk);
        }
    }

    fn latency_samples(&self) -> usize {
        if self.net.is_some() { LATENCY_SAMPLES } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the STFT framing alone adds; the rest of the latency is the model's.
    const STFT_DELAY: usize = HOP;

    #[test]
    fn stft_round_trip_delays_one_hop_and_reconstructs() {
        let mut stft = Stft::new();
        let total = HOP * 40;
        let input: Vec<f32> = (0..total)
            .map(|n| (n as f32 * 0.013).sin() * 0.5 + (n as f32 * 0.0007).sin() * 0.3)
            .collect();
        let mut out = vec![0.0; total];
        let (mut spec, mut hop) = ([0.0; SPEC_LEN], [0.0; HOP]);
        for h in 0..total / HOP {
            stft.analyse(&input[h * HOP..(h + 1) * HOP], &mut spec);
            stft.synthesise(&spec, &mut hop);
            out[h * HOP..(h + 1) * HOP].copy_from_slice(&hop);
        }
        let worst = (2 * HOP..total - STFT_DELAY)
            .map(|n| (out[n + STFT_DELAY] - input[n]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "reconstruction error {worst}");
    }

    #[test]
    fn a_missing_model_is_reported_and_audio_passes_through() {
        let section = Arc::new(section(LIGHT));
        let mut stage = DpdfNet::new(section.clone(), Variant { file: "nope.onnx", ..LIGHT });
        let mut audio = vec![0.25; HOP * 3];
        stage.process(&mut audio);
        assert_eq!(audio, vec![0.25; HOP * 3]);
        assert!(section.status().contains("nope.onnx"), "{}", section.status());
        assert!(section.open.load(std::sync::atomic::Ordering::Relaxed), "row unfolds to show it");
        assert_eq!(stage.latency_samples(), 0);
        assert!(!stage.should_look_for_model(), "searched a moment ago, so waits");
        stage.last_search = Some(Instant::now() - 2 * SEARCH_EVERY);
        assert!(stage.should_look_for_model(), "a missing model is looked for again");
        stage.broken = true;
        assert!(!stage.should_look_for_model(), "a model that failed to load is not retried");
    }

    /// Skips (returns None) unless both the model and an ONNX Runtime library can be found.
    fn real_stage() -> Option<DpdfNet> {
        if find_model(LIGHT.file).is_err() || find_runtime().is_err() {
            eprintln!("skipped: model or ONNX Runtime library not installed (set MIDDLEMIC_MODELS and ORT_DYLIB_PATH)");
            return None;
        }
        let mut stage = DpdfNet::new(Arc::new(section(LIGHT)), LIGHT);
        stage.prepare(SAMPLE_RATE);
        assert!(stage.net.is_some(), "{}", stage.section.status());
        Some(stage)
    }

    #[test]
    fn the_real_model_reports_latency_and_turns_noise_down() {
        let Some(mut stage) = real_stage() else { return };
        assert_eq!(stage.latency_samples(), LATENCY_SAMPLES);
        let mut seed = 7u32;
        let mut audio: Vec<f32> = (0..HOP * 100)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.2
            })
            .collect();
        let before: f32 = audio.iter().map(|s| s * s).sum();
        stage.process(&mut audio);
        let after: f32 = audio[HOP * 50..].iter().map(|s| s * s).sum::<f32>() * 2.0;
        assert!(audio.iter().all(|s| s.is_finite()));
        assert!(after < before * 0.5, "steady noise should be reduced: {before} -> {after}");
    }

    #[test]
    fn two_stages_in_one_process_run_independent_state() {
        let (Some(mut a), Some(mut b)) = (real_stage(), real_stage()) else { return };
        let tone = |n: usize| (n as f32 * 0.05).sin() * 0.3;
        let mut x: Vec<f32> = (0..HOP * 20).map(tone).collect();
        let mut y = x.clone();
        a.process(&mut x);
        b.process(&mut y[..HOP * 10]);
        b.process(&mut y[HOP * 10..]);
        assert_eq!(x, y, "chunking must not change the result");
    }

    #[test]
    fn the_download_is_pinned_and_checksummed() {
        assert_eq!(RUNTIME.sha256.len(), 64);
        assert!(RUNTIME.url.contains("/download/v"), "must name a release");
        assert!(RUNTIME.url.contains("win-x64") && RUNTIME.member.is_some_and(|m| m.ends_with(RUNTIME.file)));
        assert_eq!(LIGHT.sha256.len(), 64);
        assert!(LIGHT.url.ends_with(LIGHT.file));
        assert!(!LIGHT.url.contains("/main/"), "must name a revision");
    }

    /// Cost per hop on a fixed noisy clip. `MIDDLEMIC_MODELS=<dir> MIDDLEMIC_BENCH_FILE=<name>
    /// [MIDDLEMIC_BENCH_OUT=<raw f32 file>] cargo test --release bench_hop -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_hop() {
        let file: &'static str = Box::leak(
            std::env::var("MIDDLEMIC_BENCH_FILE").unwrap_or_else(|_| LIGHT.file.into()).into_boxed_str(),
        );
        let section = Arc::new(section(LIGHT));
        let mut stage = DpdfNet::new(section.clone(), Variant { file, ..LIGHT });
        stage.prepare(SAMPLE_RATE);
        assert!(stage.net.is_some(), "{}", section.status());
        let mut seed = 12345u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let hops = 1500;
        let mut audio: Vec<f32> = (0..hops * HOP)
            .map(|n| {
                let t = n as f32 / SAMPLE_RATE as f32;
                let gate = if (t * 2.0).fract() < 0.6 { 1.0 } else { 0.0 };
                let voice = (0..6).map(|k| ((k + 1) as f32 * 140.0 * t * std::f32::consts::TAU).sin() / (k + 1) as f32).sum::<f32>();
                0.2 * gate * voice + 0.05 * noise()
            })
            .collect();
        let warm = 100 * HOP;
        let (head, rest) = audio.split_at_mut(warm);
        stage.process(head);
        let start = Instant::now();
        stage.process(rest);
        let per_hop = start.elapsed().as_secs_f64() * 1e3 / (hops - 100) as f64;
        println!("BENCH {file}: {per_hop:.3} ms per 10 ms hop = {:.1}% of a core", per_hop * 10.0);
        if let Ok(path) = std::env::var("MIDDLEMIC_BENCH_OUT") {
            let bytes: Vec<u8> = audio.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(path, bytes).unwrap();
        }
    }

    #[test]
    fn starts_on_with_the_stock_reduction_cap() {
        let s = section(LIGHT);
        assert!(s.is_enabled(), "DPDFNet is the default noise removal");
        assert_eq!(s.get(0), DEFAULT_MAX_REDUCTION_DB);
    }
}
