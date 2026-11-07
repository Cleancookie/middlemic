//! Feed-forward compressor gain computer, shared by the multiband compressor and the
//! de-esser. Works in the dB domain with separate attack and release smoothing.

pub struct GainComputer {
    reduction_db: f32,
    attack: f32,
    release: f32,
}

fn time_coeff(sample_rate: f32, ms: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate)).exp()
    }
}

impl Default for GainComputer {
    fn default() -> Self {
        Self {
            reduction_db: 0.0,
            attack: 0.0,
            release: 0.0,
        }
    }
}

impl GainComputer {
    pub fn set_times(&mut self, sample_rate: f32, attack_ms: f32, release_ms: f32) {
        self.attack = time_coeff(sample_rate, attack_ms);
        self.release = time_coeff(sample_rate, release_ms);
    }

    /// Linear gain for this sample, given the detector signal. `max_reduction_db` caps it.
    pub fn gain(&mut self, detector: f32, threshold_db: f32, ratio: f32, max_reduction_db: f32) -> f32 {
        let level_db = 20.0 * detector.abs().max(1e-6).log10();
        let over = level_db - threshold_db;
        let target = if over > 0.0 {
            (over * (1.0 - 1.0 / ratio.max(1.0))).min(max_reduction_db)
        } else {
            0.0
        };
        let coeff = if target > self.reduction_db {
            self.attack
        } else {
            self.release
        };
        self.reduction_db = target + (self.reduction_db - target) * coeff;
        10f32.powf(-self.reduction_db / 20.0)
    }

    /// Current gain reduction in dB (positive).
    #[cfg(test)]
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_signal_settles_to_ratio_math() {
        let mut g = GainComputer::default();
        g.set_times(48_000.0, 1.0, 50.0);
        // 0.5 is -6.02 dB; threshold -26.02 -> 20 dB over; 4:1 -> 15 dB reduction.
        for _ in 0..48_000 {
            g.gain(0.5, -26.02, 4.0, 60.0);
        }
        assert!((g.reduction_db() - 15.0).abs() < 0.1, "got {}", g.reduction_db());
    }

    #[test]
    fn below_threshold_does_nothing() {
        let mut g = GainComputer::default();
        g.set_times(48_000.0, 1.0, 50.0);
        let gain = g.gain(0.01, -20.0, 4.0, 60.0);
        assert_eq!(gain, 1.0);
    }

    #[test]
    fn reduction_is_capped() {
        let mut g = GainComputer::default();
        g.set_times(48_000.0, 0.0, 0.0);
        g.gain(1.0, -60.0, 10.0, 8.0);
        assert!((g.reduction_db() - 8.0).abs() < 1e-4);
    }
}
