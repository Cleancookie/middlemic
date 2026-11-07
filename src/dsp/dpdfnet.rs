use super::Processor;
use super::cap::ReductionCap;
use crate::settings::{Param, Section};
use anyhow::{Context, Result, anyhow, ensure};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tract_onnx::prelude::*;

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
/// huggingface.co/Ceva-IP/DPDFNet (`onnx/<file>`). Only the light model: on `tract` it costs
/// about 40% of a core, and the quality one (`dpdfnet8_48khz_hr`) needs more than a whole core.
#[derive(Clone, Copy)]
pub struct Variant {
    pub name: &'static str,
    pub file: &'static str,
}

pub const LIGHT: Variant = Variant {
    name: "Noise removal (DPDFNet)",
    file: "dpdfnet2_48khz_hr.onnx",
};

/// Off by default: an alternative to the stock "Noise removal", for trying side by side.
pub fn section(variant: Variant) -> Section {
    Section::new(
        variant.name,
        vec![Param::new("Max reduction", "dB", 0.0, 100.0, DEFAULT_MAX_REDUCTION_DB)],
    )
    .off_by_default()
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
    if let Some(dir) = crate::settings::settings_path().parent() {
        dirs.push(dir.join("models"));
    }
    dirs
}

fn find_model(file: &str) -> Result<PathBuf> {
    let dirs = model_dirs();
    dirs.iter().map(|d| d.join(file)).find(|p| p.is_file()).ok_or_else(|| {
        let searched: Vec<_> = dirs.iter().map(|d| d.display().to_string()).collect();
        anyhow!("Model file {file} not found. Put it in one of: {}", searched.join("  |  "))
    })
}

/// The model and its recurrent state. One call per hop.
struct Net {
    plan: TypedRunnableModel<TypedModel>,
    state: Vec<f32>,
}

impl Net {
    fn load(path: &Path) -> Result<Self> {
        let onnx = tract_onnx::onnx();
        let proto = onnx
            .proto_model_for_path(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let meta = |key: &str| {
            proto.metadata_props.iter().find(|e| e.key == key).map(|e| e.value.clone())
        };
        let floats = |key: &str| -> Vec<f32> {
            meta(key)
                .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
                .unwrap_or_default()
        };
        let size = |key: &str, default: usize| {
            meta(key).and_then(|s| s.trim().parse().ok()).unwrap_or(default)
        };
        let state_size = size("state_size", 0);
        ensure!(state_size > 0, "{} has no state_size metadata", path.display());
        let (erb_len, spec_len) = (size("erb_norm_state_size", 481), size("spec_norm_state_size", 96));
        let (erb, spec) = (floats("erb_norm_init"), floats("spec_norm_init"));
        let mut init = vec![0.0; state_size];
        if erb.len() == erb_len && erb_len <= state_size {
            init[..erb_len].copy_from_slice(&erb);
        }
        if spec.len() == spec_len && erb_len + spec_len <= state_size {
            init[erb_len..erb_len + spec_len].copy_from_slice(&spec);
        }

        let plan = onnx
            .model_for_proto_model(&proto)?
            .with_input_fact(0, f32::fact([1, 1, BINS, 2]).into())?
            .with_input_fact(1, f32::fact([state_size]).into())?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self { plan, state: init })
    }

    fn run(&mut self, spec: &[f32; SPEC_LEN], spec_e: &mut [f32; SPEC_LEN]) -> Result<()> {
        let inputs = tvec!(
            Tensor::from_shape(&[1, 1, BINS, 2], spec)?.into(),
            Tensor::from_shape(&[self.state.len()], &self.state)?.into()
        );
        let out = self.plan.run(inputs)?;
        let enhanced = out[0].as_slice::<f32>()?;
        let next = out[1].as_slice::<f32>()?;
        ensure!(
            enhanced.len() == SPEC_LEN && next.len() == self.state.len(),
            "model outputs are {} and {} values, expected {SPEC_LEN} and {}",
            enhanced.len(),
            next.len(),
            self.state.len()
        );
        spec_e.copy_from_slice(enhanced);
        self.state.copy_from_slice(next);
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
    failed: bool,
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
            failed: false,
            stft: Stft::new(),
            cap,
            cap_db,
            spec: [0.0; SPEC_LEN],
            spec_e: [0.0; SPEC_LEN],
            original: [0.0; HOP],
        }
    }

    fn load(&mut self) {
        let loaded = find_model(self.variant.file).and_then(|path| Net::load(&path));
        match loaded {
            Ok(net) => {
                self.net = Some(net);
                self.section.set_status("");
            }
            Err(err) => {
                self.failed = true;
                self.section.set_status(format!("{err:#}"));
            }
        }
    }
}

impl Processor for DpdfNet {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "DPDFNet needs 48 kHz");
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % HOP, 0);
        if self.net.is_none() && !self.failed {
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
        let mut stage = DpdfNet::new(section.clone(), Variant { name: LIGHT.name, file: "nope.onnx" });
        let mut audio = vec![0.25; HOP * 3];
        stage.process(&mut audio);
        assert_eq!(audio, vec![0.25; HOP * 3]);
        assert!(section.status().contains("nope.onnx"), "{}", section.status());
        assert!(section.open.load(std::sync::atomic::Ordering::Relaxed), "row unfolds to show it");
        assert_eq!(stage.latency_samples(), 0);
    }

    #[test]
    fn starts_off_with_the_stock_reduction_cap() {
        let s = section(LIGHT);
        assert!(!s.is_enabled());
        assert_eq!(s.get(0), DEFAULT_MAX_REDUCTION_DB);
    }
}
