use super::Processor;
use crate::settings::{Param, Section};
use std::sync::Arc;

pub const NAME: &str = "Limiter";

const RELEASE_MS: f32 = 80.0;

pub fn section() -> Section {
    Section::new(NAME, vec![Param::new("Ceiling", "dB", -12.0, 0.0, -1.0)])
}

/// Safety limiter: instant attack, smooth release. Keeps boosted mics from clipping.
pub struct Limiter {
    section: Arc<Section>,
    release_coeff: f32,
    gain: f32,
}

impl Limiter {
    pub fn new(section: Arc<Section>) -> Self {
        let mut limiter = Self {
            section,
            release_coeff: 0.0,
            gain: 1.0,
        };
        limiter.prepare(48_000);
        limiter
    }
}

impl Processor for Limiter {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        let release_samples = RELEASE_MS * 0.001 * sample_rate as f32;
        self.release_coeff = (-1.0 / release_samples).exp();
    }

    fn process(&mut self, samples: &mut [f32]) {
        let ceiling = 10f32.powf(self.section.get(0) / 20.0);
        for s in samples {
            let peak = s.abs();
            let target = if peak > ceiling {
                ceiling / peak
            } else {
                1.0
            };
            self.gain = if target < self.gain {
                target
            } else {
                target + (self.gain - target) * self.release_coeff
            };
            *s *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_ceiling() {
        let s = Arc::new(section());
        s.params[0].set(-3.0);
        let mut l = Limiter::new(s);
        let ceiling = 10f32.powf(-3.0 / 20.0);
        let mut buf: Vec<f32> = (0..4800)
            .map(|i| if i % 97 == 0 { 1.0 } else { 0.9 })
            .collect();
        l.process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() <= ceiling + 1e-6));
    }

    #[test]
    fn quiet_signal_passes_untouched() {
        let mut l = Limiter::new(Arc::new(section()));
        let mut buf = vec![0.1_f32; 480];
        l.process(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.1).abs() < 1e-6));
    }
}
