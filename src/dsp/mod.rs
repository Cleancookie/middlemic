//! Platform-independent DSP. Every stage implements [`Processor`]; a [`Chain`]
//! runs them in order, so stages can be reordered, swapped or duplicated.

mod biquad;
mod compressor;
mod deepfilter;
mod deesser;
mod eq;
mod gain;
mod limiter;
mod multiband;

pub use deepfilter::DeepFilter;
#[allow(unused_imports)] // Used by the Windows-only window.
pub use deesser::{DeEsser, NAME as DEESSER_NAME, frequency_hz as deesser_frequency_hz};
pub use eq::{BANDS, Eq, NAME as EQ_NAME, response_db};
pub use gain::Gain;
pub use limiter::Limiter;
pub use multiband::Multiband;

use crate::settings::{Section, Settings};
use anyhow::Result;

/// Every stage's controls, in signal order.
pub fn default_sections() -> Vec<Section> {
    vec![
        deepfilter::section(),
        gain::section(),
        eq::section(),
        deesser::section(),
        multiband::section(),
        limiter::section(),
    ]
}

/// Build the chain in signal order. Call on the thread that will run it (see [`Processor`]).
pub fn build_chain(settings: &Settings) -> Result<Chain> {
    let mut chain = Chain::new();
    chain
        .push(DeepFilter::new(settings.section(deepfilter::NAME))?)
        .push(Gain::new(settings.section(gain::NAME)))
        .push(Eq::new(settings.section(eq::NAME)))
        .push(DeEsser::new(settings.section(deesser::NAME)))
        .push(Multiband::new(settings.section(multiband::NAME)))
        .push(Limiter::new(settings.section(limiter::NAME)));
    Ok(chain)
}

/// One stage of the mic chain. Mono `f32` samples, processed in place.
/// Not `Send`: DeepFilterNet holds `Rc`. Build the chain on the thread that runs it.
pub trait Processor {
    /// A disabled stage is skipped entirely.
    fn enabled(&self) -> bool {
        true
    }

    /// Called once before processing, and again if the sample rate changes.
    fn prepare(&mut self, _sample_rate: u32) {}

    fn process(&mut self, samples: &mut [f32]);

    /// Extra delay this stage adds, in samples.
    fn latency_samples(&self) -> usize {
        0
    }
}

#[derive(Default)]
pub struct Chain {
    stages: Vec<Box<dyn Processor>>,
}

impl Chain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, processor: impl Processor + 'static) -> &mut Self {
        self.stages.push(Box::new(processor));
        self
    }

    pub fn prepare(&mut self, sample_rate: u32) {
        for stage in &mut self.stages {
            stage.prepare(sample_rate);
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for stage in self.stages.iter_mut().filter(|s| s.enabled()) {
            stage.process(samples);
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.stages
            .iter()
            .filter(|s| s.enabled())
            .map(|s| s.latency_samples())
            .sum()
    }
}

#[cfg(test)]
pub mod testutil {
    pub fn sine(freq: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin())
            .collect()
    }

    pub fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use std::sync::atomic::Ordering::Relaxed;

    fn boosted(db: f32) -> Gain {
        let s = Arc::new(gain::section());
        s.params[0].set(db);
        Gain::new(s)
    }

    #[test]
    fn stages_run_in_order() {
        let ceiling = Arc::new(limiter::section());
        ceiling.params[0].set(-6.0);
        let mut chain = Chain::new();
        chain.push(boosted(6.0)).push(Limiter::new(ceiling));
        chain.prepare(48_000);
        let mut buf = vec![0.5_f32; 4_800];
        chain.process(&mut buf);
        let max = 10f32.powf(-6.0 / 20.0);
        assert!(buf.iter().all(|s| s.abs() <= max + 1e-6));
    }

    #[test]
    fn disabled_stage_is_skipped() {
        let s = Arc::new(gain::section());
        s.params[0].set(20.0);
        s.enabled.store(false, Relaxed);
        let mut chain = Chain::new();
        chain.push(Gain::new(s));
        let mut buf = vec![0.1_f32; 8];
        chain.process(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.1).abs() < 1e-9));
    }

    #[test]
    fn same_stage_can_be_used_twice() {
        let mut chain = Chain::new();
        chain.push(boosted(6.0)).push(boosted(6.0));
        chain.prepare(48_000);
        let mut buf = vec![0.1_f32; 48_000];
        chain.process(&mut buf);
        let expected = 0.1 * 10f32.powf(12.0 / 20.0);
        assert!((buf[47_999] - expected).abs() < 1e-3);
    }

    #[test]
    fn full_chain_builds_and_passes_a_voice_band_tone() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings).expect("chain builds");
        chain.prepare(48_000);
        let mut tone = testutil::sine(500.0, 48_000, 0.05);
        chain.process(&mut tone);
        assert!(tone.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn full_chain_cost_is_a_small_fraction_of_real_time() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings).expect("chain builds");
        chain.prepare(48_000);
        let mut audio = testutil::sine(300.0, 48_000 * 5, 0.2);
        let started = std::time::Instant::now();
        for frame in audio.chunks_exact_mut(480) {
            chain.process(frame);
        }
        let factor = started.elapsed().as_secs_f32() / 5.0;
        eprintln!("full chain real-time factor: {factor:.3} (1.0 = one full core)");
        assert!(factor < 0.25, "chain too heavy: {factor}");
    }
}
