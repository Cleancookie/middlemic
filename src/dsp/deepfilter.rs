use super::Processor;
use crate::settings::{Param, Section};
use anyhow::{Result, ensure};
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{Array2, ArrayView2};
use std::sync::Arc;

pub const NAME: &str = "Noise removal";

const SAMPLE_RATE: u32 = 48_000;
const HOP: usize = 480;
/// Measured by cross-correlating output against input: 1440 samples, 30 ms.
const LATENCY_SAMPLES: usize = 3 * HOP;

/// With no cap the model mutes soft word endings completely (measured on a real quiet
/// recording: speech frames turned down by up to 100 dB, which sounds choppy). 18 dB
/// keeps them while still cutting steady background noise by about 16 dB.
const DEFAULT_MAX_REDUCTION_DB: f32 = 18.0;

pub fn section() -> Section {
    Section::new(
        NAME,
        vec![Param::new("Max reduction", "dB", 0.0, 100.0, DEFAULT_MAX_REDUCTION_DB).help(
            "The most noise this can turn down, in dB (decibels: about 6 dB is half the loudness). \
             Start at 18. Raise it for a noisy room; lower it if soft word endings sound chopped. 100 means no limit.",
        )],
    )
    .help(
        "An older AI noise remover (DeepFilterNet), built in with no download. Lighter on CPU and adds about 30 ms \
         of delay, but cuts soft speech more than DPDFNet. Off by default.",
    )
    .off_by_default()
}

/// DeepFilterNet3 noise suppression. Expects 48 kHz mono, in multiples of 480 samples.
/// "Max reduction" caps how much noise is removed. Raise it for noisier rooms; lower it if
/// words sound clipped.
pub struct DeepFilter {
    section: Arc<Section>,
    model: DfTract,
    out: Array2<f32>,
    atten_lim_db: f32,
}

// SAFETY: `DfTract` is not `Send` only because tract keeps intermediate tensors in `Rc`. Every
// such `Rc` is created inside this model and no clone is handed out, so moving the whole stage
// moves every reference to them; it is never used from two threads at once.
unsafe impl Send for DeepFilter {}

impl DeepFilter {
    pub fn new(section: Arc<Section>) -> Result<Self> {
        let atten_lim_db = section.get(0);
        // Stock settings skip the deep filter above 20 dB local SNR. The mask alone then cuts
        // loud speech by about 11 dB until the estimate falls back, which sounds like a stutter.
        let stock = RuntimeParams::default_with_ch(1);
        let (min_db, max_db) = (stock.min_db_thresh, stock.max_db_erb_thresh);
        let params = stock.with_atten_lim(atten_lim_db).with_thresholds(min_db, max_db, max_db);
        let model = DfTract::new(DfParams::default(), &params)?;
        ensure!(
            (model.sr, model.hop_size, model.ch) == (SAMPLE_RATE as usize, HOP, 1),
            "unexpected model format: {} Hz, hop {}, {} ch",
            model.sr,
            model.hop_size,
            model.ch
        );
        Ok(Self {
            section,
            model,
            out: Array2::zeros((1, HOP)),
            atten_lim_db,
        })
    }
}

impl Processor for DeepFilter {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "DeepFilterNet needs 48 kHz");
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % HOP, 0);
        let limit = self.section.get(0);
        if limit != self.atten_lim_db {
            self.atten_lim_db = limit;
            self.model.set_atten_lim(limit);
        }
        for chunk in samples.chunks_exact_mut(HOP) {
            let input = ArrayView2::from_shape((1, HOP), chunk).expect("chunk is one hop");
            if self.model.process(input, self.out.view_mut()).is_ok() {
                chunk.copy_from_slice(self.out.as_slice().expect("contiguous"));
            }
        }
    }

    fn latency_samples(&self) -> usize {
        LATENCY_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Deterministic white noise, no extra dependency.
    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut state = 0x1234_5678_u32;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    #[test]
    fn removes_steady_noise() {
        let mut df = DeepFilter::new(Arc::new(section())).expect("model loads");
        df.prepare(48_000);
        let mut audio = noise(HOP * 300, 0.1);
        let tail_before = rms(&audio[HOP * 200..]);
        let started = std::time::Instant::now();
        df.process(&mut audio);
        let audio_secs = audio.len() as f32 / SAMPLE_RATE as f32;
        eprintln!(
            "real-time factor: {:.3} (1.0 = one full core)",
            started.elapsed().as_secs_f32() / audio_secs
        );
        let tail_after = rms(&audio[HOP * 200..]);
        assert!(
            tail_after < tail_before * 0.25,
            "noise {tail_before} -> {tail_after}, expected a clear reduction"
        );
    }

    #[test]
    fn mask_never_runs_without_the_deep_filter() {
        let df = DeepFilter::new(Arc::new(section())).expect("model loads");
        for lsnr in -20..=40 {
            let (mask, _, deep_filter) = df.model.apply_stages(lsnr as f32);
            assert!(!mask || deep_filter, "mask alone at {lsnr} dB local SNR");
        }
    }

    #[test]
    fn output_is_finite() {
        let s = Arc::new(section());
        s.params[0].set(30.0);
        let mut df = DeepFilter::new(s).expect("model loads");
        let mut audio = noise(HOP * 20, 0.5);
        df.process(&mut audio);
        assert!(audio.iter().all(|s| s.is_finite()));
    }
}
