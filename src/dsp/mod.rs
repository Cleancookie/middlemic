//! Platform-independent DSP. Every stage implements [`Processor`]; a [`Chain`]
//! runs them in order, so stages can be reordered, swapped or duplicated.

mod biquad;
mod cap;
mod compressor;
mod dpdfnet;
mod deepfilter;
mod deesser;
mod echo;
mod eq;
mod gain;
mod limiter;
mod multiband;
mod rnnoise;

pub use deepfilter::DeepFilter;
#[allow(unused_imports)] // The download button is in the Windows-only window.
pub use dpdfnet::{DpdfNet, LIGHT as DPDFNET, download_dir as model_download_dir, model_installed};
pub use echo::EchoCancel;
#[allow(unused_imports)] // Used by the Windows-only window.
pub use deesser::{DeEsser, NAME as DEESSER_NAME, frequency_hz as deesser_frequency_hz};
pub use eq::{BANDS, Eq, NAME as EQ_NAME, response_db};
pub use gain::Gain;
pub use limiter::Limiter;
pub use multiband::Multiband;
pub use rnnoise::RnNoise;

use crate::settings::{Section, Settings};
use anyhow::Result;
use ringbuf::HeapCons;

/// Every stage's controls, in signal order.
pub fn default_sections() -> Vec<Section> {
    vec![
        echo::section(),
        deepfilter::section(),
        dpdfnet::section(dpdfnet::LIGHT),
        rnnoise::section(),
        gain::section(),
        eq::section(),
        deesser::section(),
        multiband::section(),
        limiter::section(),
    ]
}

/// Build one stage for each of the settings' sections, then run them in the saved order.
/// A stage's index in the chain is its section's index, which is what `Settings::order` uses.
/// `reference` is the audio going to the speakers, for echo cancellation; with `None` that
/// stage never runs. Call on the thread that will run it (see [`Processor`]).
pub fn build_chain(settings: &Settings, reference: Option<HeapCons<f32>>) -> Result<Chain> {
    let mut chain = Chain::new();
    let mut reference = reference;
    for section in &settings.sections {
        let section = section.clone();
        match section.name {
            echo::NAME => chain.push(EchoCancel::new(section, reference.take())),
            name if name == dpdfnet::LIGHT.name => chain.push(DpdfNet::new(section, dpdfnet::LIGHT)),
            rnnoise::NAME => chain.push(RnNoise::new(section)),
            deepfilter::NAME => chain.push(DeepFilter::new(section)?),
            gain::NAME => chain.push(Gain::new(section)),
            eq::NAME => chain.push(Eq::new(section)),
            deesser::NAME => chain.push(DeEsser::new(section)),
            multiband::NAME => chain.push(Multiband::new(section)),
            limiter::NAME => chain.push(Limiter::new(section)),
            other => anyhow::bail!("no stage is known for the section {other:?}"),
        };
    }
    chain.set_order(&settings.order.get());
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
    /// Which stage runs at each position. Stages keep their state when this changes.
    order: Vec<usize>,
}

impl Chain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, processor: impl Processor + 'static) -> &mut Self {
        self.order.push(self.stages.len());
        self.stages.push(Box::new(processor));
        self
    }

    /// Run the stages in this order (indexes in the order they were pushed). Returns false
    /// and changes nothing unless it lists every stage exactly once.
    pub fn set_order(&mut self, order: &[usize]) -> bool {
        let valid = order.len() == self.stages.len() && crate::settings::is_permutation(order);
        if valid {
            self.order.clear();
            self.order.extend_from_slice(order);
        }
        valid
    }

    pub fn prepare(&mut self, sample_rate: u32) {
        for stage in &mut self.stages {
            stage.prepare(sample_rate);
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for &i in &self.order {
            let stage = &mut self.stages[i];
            if stage.enabled() {
                stage.process(samples);
            }
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
    fn order_decides_what_the_audio_does() {
        let ceiling = Arc::new(limiter::section());
        ceiling.params[0].set(-6.0); // about 0.5
        let mut chain = Chain::new();
        chain.push(boosted(6.0)).push(Limiter::new(ceiling));
        chain.prepare(48_000);

        let mut limited_last = vec![0.4_f32; 48_000];
        chain.process(&mut limited_last);
        assert!(limited_last[47_999] <= 0.51, "boost then limit stays under the ceiling");

        // The limiter keeps its state across the change and takes a moment to ease off.
        assert!(chain.set_order(&[1, 0]));
        let mut limited_first = vec![0.4_f32; 48_000];
        chain.process(&mut limited_first);
        assert!(limited_first[47_999] > 0.75, "limit then boost is louder: {}", limited_first[47_999]);
    }

    #[test]
    fn a_bad_order_changes_nothing() {
        let mut chain = Chain::new();
        chain.push(boosted(0.0)).push(boosted(0.0)).push(boosted(0.0));
        assert!(!chain.set_order(&[0, 1]), "too short");
        assert!(!chain.set_order(&[0, 1, 1]), "repeat");
        assert!(!chain.set_order(&[0, 1, 5]), "out of range");
        assert_eq!(chain.order, vec![0, 1, 2]);
        assert!(chain.set_order(&[2, 0, 1]));
    }

    #[test]
    fn the_built_chain_starts_in_the_saved_order() {
        let settings = Settings::new(default_sections());
        let reversed: Vec<usize> = (0..settings.sections.len()).rev().collect();
        settings.order.set(&reversed);
        let chain = build_chain(&settings, None).unwrap();
        assert_eq!(chain.order, reversed);
    }

    #[test]
    fn every_default_section_has_a_stage_and_unknown_ones_are_an_error() {
        assert!(build_chain(&Settings::new(default_sections()), None).is_ok());
        let odd = Settings::new(vec![crate::settings::Section::new("Mystery", vec![])]);
        let message = build_chain(&odd, None).err().expect("unknown section").to_string();
        assert!(message.contains("Mystery"), "{message}");
    }

    #[test]
    fn full_chain_builds_and_passes_a_voice_band_tone() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings, None).expect("chain builds");
        chain.prepare(48_000);
        let mut tone = testutil::sine(500.0, 48_000, 0.05);
        chain.process(&mut tone);
        assert!(tone.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn full_chain_cost_is_a_small_fraction_of_real_time() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings, None).expect("chain builds");
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
