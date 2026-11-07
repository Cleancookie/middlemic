use super::Processor;
use anyhow::{Result, ensure};
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::{Array2, ArrayView2};

const SAMPLE_RATE: u32 = 48_000;
const HOP: usize = 480;
/// 20 ms STFT window plus 20 ms lookahead. Estimated from the model config, not measured.
const LATENCY_SAMPLES: usize = 4 * HOP;

/// DeepFilterNet3 noise suppression. Expects 48 kHz mono, in multiples of 480 samples.
pub struct DeepFilter {
    model: DfTract,
    out: Array2<f32>,
}

impl DeepFilter {
    /// `atten_lim_db` caps how much noise is removed; `None` removes as much as it can.
    /// A limit of 25-40 dB tends to sound more natural than full suppression.
    pub fn new(atten_lim_db: Option<f32>) -> Result<Self> {
        let mut params = RuntimeParams::default_with_ch(1);
        if let Some(db) = atten_lim_db {
            params = params.with_atten_lim(db);
        }
        let model = DfTract::new(DfParams::default(), &params)?;
        ensure!(
            (model.sr, model.hop_size, model.ch) == (SAMPLE_RATE as usize, HOP, 1),
            "unexpected model format: {} Hz, hop {}, {} ch",
            model.sr,
            model.hop_size,
            model.ch
        );
        Ok(Self {
            model,
            out: Array2::zeros((1, HOP)),
        })
    }
}

impl Processor for DeepFilter {
    fn name(&self) -> &str {
        "deepfilternet3"
    }

    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "DeepFilterNet needs 48 kHz");
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % HOP, 0);
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
        let mut df = DeepFilter::new(None).expect("model loads");
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
    fn output_is_finite() {
        let mut df = DeepFilter::new(Some(30.0)).expect("model loads");
        let mut audio = noise(HOP * 20, 0.5);
        df.process(&mut audio);
        assert!(audio.iter().all(|s| s.is_finite()));
    }
}
