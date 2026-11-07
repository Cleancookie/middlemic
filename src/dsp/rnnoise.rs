use super::Processor;
use super::cap::ReductionCap;
use crate::settings::{Param, Section};
use nnnoiseless::{DenoiseState, FRAME_SIZE};
use std::sync::Arc;

pub const NAME: &str = "Noise removal (RNNoise)";

const SAMPLE_RATE: u32 = 48_000;
/// RNNoise works on 480-sample frames at the scale of 16-bit samples.
const PCM_SCALE: f32 = 32768.0;
/// Measured by cross-correlating output against input (`compare_denoisers` in `offline.rs`).
const LATENCY_SAMPLES: usize = FRAME_SIZE;
const DEFAULT_MAX_REDUCTION_DB: f32 = 18.0;

/// Off by default: a very cheap alternative to the neural stages, for trying side by side.
pub fn section() -> Section {
    Section::new(
        NAME,
        vec![Param::new("Max reduction", "dB", 0.0, 100.0, DEFAULT_MAX_REDUCTION_DB).help(
            "The most noise this can turn down, in dB (decibels: about 6 dB is half the loudness). \
             Start at 18. Raise it for a noisy room; lower it if your voice sounds warbly or chopped. 100 means no limit.",
        )],
    )
    .help(
        "A very light, older noise remover (RNNoise) that costs almost no CPU. It removes less than the AI options and can \
         warble on speech. Adds about 10 ms of delay. Try it on a slow PC. Off by default.",
    )
    .off_by_default()
}

/// RNNoise (Xiph), in Rust. Old and tiny next to DeepFilterNet, so it removes less noise and
/// can warble on speech, but it costs almost nothing.
pub struct RnNoise {
    section: Arc<Section>,
    state: Box<DenoiseState<'static>>,
    cap: ReductionCap,
    cap_db: f32,
    scaled_in: [f32; FRAME_SIZE],
    scaled_out: [f32; FRAME_SIZE],
    original: [f32; FRAME_SIZE],
}

impl RnNoise {
    pub fn new(section: Arc<Section>) -> Self {
        let mut cap = ReductionCap::new(LATENCY_SAMPLES);
        let cap_db = section.get(0);
        cap.set_db(cap_db);
        Self {
            section,
            state: DenoiseState::new(),
            cap,
            cap_db,
            scaled_in: [0.0; FRAME_SIZE],
            scaled_out: [0.0; FRAME_SIZE],
            original: [0.0; FRAME_SIZE],
        }
    }
}

impl Processor for RnNoise {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "RNNoise needs 48 kHz");
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % FRAME_SIZE, 0);
        let db = self.section.get(0);
        if db != self.cap_db {
            self.cap_db = db;
            self.cap.set_db(db);
        }
        for chunk in samples.chunks_exact_mut(FRAME_SIZE) {
            self.original.copy_from_slice(chunk);
            for (scaled, s) in self.scaled_in.iter_mut().zip(chunk.iter()) {
                *scaled = s * PCM_SCALE;
            }
            self.state.process_frame(&mut self.scaled_out, &self.scaled_in);
            for (s, scaled) in chunk.iter_mut().zip(&self.scaled_out) {
                *s = scaled / PCM_SCALE;
            }
            self.cap.apply(&self.original, chunk);
        }
    }

    fn latency_samples(&self) -> usize {
        LATENCY_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_off() {
        assert!(!section().is_enabled());
    }

    /// With the reduction cap at 0 dB the output is the input again, so this pins the stage's
    /// latency constant: a wrong one would shift the copy against the model's own delay.
    #[test]
    fn zero_db_cap_returns_the_input_after_the_reported_latency() {
        let section = Arc::new(section());
        section.params[0].set(0.0);
        let mut stage = RnNoise::new(section);
        let input: Vec<f32> = (0..FRAME_SIZE * 20).map(|n| (n as f32 * 0.05).sin() * 0.2).collect();
        let mut out = input.clone();
        stage.process(&mut out);
        let delay = stage.latency_samples();
        assert!((delay..input.len()).all(|n| (out[n] - input[n - delay]).abs() < 1e-6));
    }
}
