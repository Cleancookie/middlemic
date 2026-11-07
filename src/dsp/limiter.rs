use super::Processor;

/// Safety limiter: instant attack, smooth release. Keeps boosted mics from clipping.
pub struct Limiter {
    ceiling: f32,
    release_ms: f32,
    release_coeff: f32,
    gain: f32,
}

impl Limiter {
    pub fn new(ceiling_db: f32) -> Self {
        let mut limiter = Self {
            ceiling: 10f32.powf(ceiling_db / 20.0),
            release_ms: 80.0,
            release_coeff: 0.0,
            gain: 1.0,
        };
        limiter.prepare(48_000);
        limiter
    }
}

impl Processor for Limiter {
    fn name(&self) -> &str {
        "limiter"
    }

    fn prepare(&mut self, sample_rate: u32) {
        let release_samples = self.release_ms * 0.001 * sample_rate as f32;
        self.release_coeff = (-1.0 / release_samples).exp();
    }

    fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            let peak = s.abs();
            let target = if peak > self.ceiling {
                self.ceiling / peak
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
        let mut l = Limiter::new(-3.0);
        let ceiling = 10f32.powf(-3.0 / 20.0);
        let mut buf: Vec<f32> = (0..4800)
            .map(|i| if i % 97 == 0 { 1.0 } else { 0.9 })
            .collect();
        l.process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() <= ceiling + 1e-6));
    }

    #[test]
    fn quiet_signal_passes_untouched() {
        let mut l = Limiter::new(-1.0);
        let mut buf = vec![0.1_f32; 480];
        l.process(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.1).abs() < 1e-6));
    }
}
