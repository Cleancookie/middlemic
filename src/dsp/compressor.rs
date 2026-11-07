//! Feed-forward compressor building blocks, shared by the multiband compressor and the
//! de-esser. Everything works in the dB domain.

fn time_coeff(sample_rate: f32, ms: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate)).exp()
    }
}

/// `max(x, 0)` with the corner rounded over `knee` dB.
fn soft_positive(x: f32, knee: f32) -> f32 {
    let half = knee * 0.5;
    if knee <= 0.0 || x >= half {
        x.max(0.0)
    } else if x <= -half {
        0.0
    } else {
        (x + half) * (x + half) / (2.0 * knee)
    }
}

/// A static gain curve: compression above `threshold` and downward expansion below `floor`,
/// both with the same soft knee.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    pub threshold: f32,
    /// `ratio`:1 above the threshold.
    pub ratio: f32,
    pub knee: f32,
    pub floor: f32,
    /// 1:`floor_ratio` below the floor; 1 is off.
    pub floor_ratio: f32,
}

impl Curve {
    /// How far a steady level above the threshold is turned down, in dB (positive).
    pub fn compression_db(&self, level_db: f32) -> f32 {
        soft_positive(level_db - self.threshold, self.knee) * (1.0 - 1.0 / self.ratio.max(1.0))
    }

    /// How far a steady level below the floor is turned down, in dB (positive).
    pub fn expansion_db(&self, level_db: f32) -> f32 {
        soft_positive(self.floor - level_db, self.knee) * (self.floor_ratio.max(1.0) - 1.0)
    }

    pub fn output_db(&self, level_db: f32) -> f32 {
        level_db - self.compression_db(level_db) - self.expansion_db(level_db)
    }

    /// True when it never changes the level.
    pub fn is_flat(&self) -> bool {
        self.ratio <= 1.0 && self.floor_ratio <= 1.0
    }
}

/// One-pole smoothing: rising uses the attack time, falling waits out the hold, then uses
/// the release time.
#[derive(Default)]
pub struct Envelope {
    value: f32,
    attack: f32,
    release: f32,
    hold: u32,
    held: u32,
}

impl Envelope {
    pub fn set_times(&mut self, sample_rate: f32, attack_ms: f32, hold_ms: f32, release_ms: f32) {
        self.attack = time_coeff(sample_rate, attack_ms);
        self.release = time_coeff(sample_rate, release_ms);
        self.hold = (hold_ms.max(0.0) * 0.001 * sample_rate) as u32;
    }

    pub fn tick(&mut self, target: f32) -> f32 {
        let coeff = if target > self.value {
            self.held = self.hold;
            self.attack
        } else if self.held > 0 {
            self.held -= 1;
            return self.value;
        } else {
            self.release
        };
        self.value = target + (self.value - target) * coeff;
        self.value
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn clear(&mut self) {
        self.value = 0.0;
        self.held = 0;
    }
}

/// Hard-knee gain computer with attack and release, as the de-esser uses it.
#[derive(Default)]
pub struct GainComputer {
    envelope: Envelope,
}

impl GainComputer {
    pub fn set_times(&mut self, sample_rate: f32, attack_ms: f32, release_ms: f32) {
        self.envelope.set_times(sample_rate, attack_ms, 0.0, release_ms);
    }

    /// Linear gain for this sample, given the detector signal. `max_reduction_db` caps it.
    pub fn gain(&mut self, detector: f32, threshold_db: f32, ratio: f32, max_reduction_db: f32) -> f32 {
        let level_db = 20.0 * detector.abs().max(1e-6).log10();
        let curve = Curve { threshold: threshold_db, ratio, knee: 0.0, floor: f32::NEG_INFINITY, floor_ratio: 1.0 };
        let reduction = self.envelope.tick(curve.compression_db(level_db).min(max_reduction_db));
        db_to_gain(-reduction)
    }

    /// Current gain reduction in dB (positive).
    pub fn reduction_db(&self) -> f32 {
        self.envelope.value()
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
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

    fn curve(ratio: f32, knee: f32) -> Curve {
        Curve { threshold: -20.0, ratio, knee, floor: -60.0, floor_ratio: 1.0 }
    }

    #[test]
    fn hard_knee_follows_the_ratio_above_the_threshold() {
        let c = curve(4.0, 0.0);
        assert_eq!(c.output_db(-30.0), -30.0);
        assert_eq!(c.output_db(-20.0), -20.0);
        assert!((c.output_db(0.0) - -15.0).abs() < 1e-4, "20 dB over at 4:1 comes out 5 dB over");
        assert!((curve(20.0, 0.0).output_db(0.0) - -19.0).abs() < 1e-4);
        assert!(curve(1.0, 6.0).is_flat());
    }

    #[test]
    fn soft_knee_bends_smoothly_and_meets_the_hard_curve_outside_it() {
        let (soft, hard) = (curve(4.0, 12.0), curve(4.0, 0.0));
        assert_eq!(soft.output_db(-27.0), -27.0, "below the knee");
        assert!((soft.output_db(-13.0) - hard.output_db(-13.0)).abs() < 1e-4, "above the knee");
        // At the threshold itself: a quarter of the knee width times (1 - 1/ratio) / 2.
        assert!((soft.compression_db(-20.0) - 12.0 / 8.0 * 0.75).abs() < 1e-4);
        let mut last = f32::NEG_INFINITY;
        for i in 0..400 {
            let out = soft.output_db(-40.0 + i as f32 * 0.1);
            assert!(out > last, "monotonic");
            last = out;
        }
    }

    #[test]
    fn the_floor_pushes_quiet_levels_further_down() {
        let c = Curve { floor_ratio: 3.0, ..curve(1.0, 0.0) };
        assert_eq!(c.output_db(-50.0), -50.0, "above the floor");
        assert!((c.output_db(-70.0) - -90.0).abs() < 1e-4, "10 dB under at 1:3 comes out 30 dB under");
        assert!(!c.is_flat());
    }

    #[test]
    fn hold_delays_the_release() {
        let mut e = Envelope::default();
        e.set_times(1_000.0, 0.0, 5.0, 0.0);
        e.tick(10.0);
        for _ in 0..5 {
            assert_eq!(e.tick(0.0), 10.0, "held");
        }
        assert_eq!(e.tick(0.0), 0.0, "then released");
    }
}
