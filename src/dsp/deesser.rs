use super::Processor;
use super::biquad::Crossover;
use super::compressor::GainComputer;
use crate::settings::{Param, Section};
use std::sync::Arc;

pub const NAME: &str = "De-esser";

const FREQ: usize = 0;
const THRESHOLD: usize = 1;
const MAX_REDUCTION: usize = 2;

const RATIO: f32 = 6.0;

pub fn section() -> Section {
    Section::new(
        NAME,
        vec![
            Param::new("Frequency", "Hz", 3000.0, 10000.0, 6500.0).log(),
            Param::new("Threshold", "dB", -60.0, 0.0, -30.0),
            Param::new("Max reduction", "dB", 0.0, 20.0, 8.0),
        ],
    )
    .with_reduction_meters(&["Sibilance"])
}

/// Split-band: only the sibilance band above `Frequency` is turned down.
pub struct DeEsser {
    section: Arc<Section>,
    fs: f32,
    split: Crossover,
    comp: GainComputer,
}

impl DeEsser {
    pub fn new(section: Arc<Section>) -> Self {
        let mut d = Self {
            section,
            fs: 48_000.0,
            split: Crossover::default(),
            comp: GainComputer::default(),
        };
        d.prepare(48_000);
        d
    }
}

impl Processor for DeEsser {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        self.fs = sample_rate as f32;
        self.comp.set_times(self.fs, 1.0, 40.0);
        self.split = Crossover::default();
    }

    fn process(&mut self, samples: &mut [f32]) {
        let threshold = self.section.get(THRESHOLD);
        let max_reduction = self.section.get(MAX_REDUCTION);
        self.split.set_freq(self.fs, self.section.get(FREQ));
        let mut worst = 0f32;
        for s in samples {
            let (low, high) = self.split.split(*s);
            *s = low + high * self.comp.gain(high, threshold, RATIO, max_reduction);
            worst = worst.max(self.comp.reduction_db());
        }
        self.section.record_reduction(0, worst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};

    fn level_after(freq: f32) -> f32 {
        let mut d = DeEsser::new(Arc::new(section()));
        let mut tone = sine(freq, 48_000, 0.3);
        d.process(&mut tone);
        rms(&tone[24_000..]) / rms(&sine(freq, 48_000, 0.3)[24_000..])
    }

    #[test]
    fn loud_sibilance_is_reduced_voice_is_not() {
        assert!(level_after(8_000.0) < 0.6, "8 kHz {}", level_after(8_000.0));
        assert!(level_after(300.0) > 0.97, "300 Hz {}", level_after(300.0));
    }

    #[test]
    fn meter_shows_the_reduction_it_applied() {
        let s = Arc::new(section());
        let mut d = DeEsser::new(s.clone());
        d.process(&mut sine(8_000.0, 48_000, 0.3));
        let loud = s.take_reduction(0);
        assert!(loud > 3.0 && loud <= 8.01, "{loud}");

        d.process(&mut sine(300.0, 48_000, 0.3));
        d.process(&mut sine(300.0, 48_000, 0.3));
        s.take_reduction(0);
        d.process(&mut sine(300.0, 4_800, 0.3));
        assert!(s.take_reduction(0) < 0.5);
    }

    #[test]
    fn quiet_sibilance_is_left_alone() {
        let mut d = DeEsser::new(Arc::new(section()));
        let mut tone = sine(8_000.0, 48_000, 0.005);
        d.process(&mut tone);
        let ratio = rms(&tone[24_000..]) / rms(&sine(8_000.0, 48_000, 0.005)[24_000..]);
        assert!(ratio > 0.97, "{ratio}");
    }
}
