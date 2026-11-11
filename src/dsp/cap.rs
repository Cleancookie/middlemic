/// "Max reduction" for stages that have no such control of their own: mixes the stage's input
/// back into its output so noise can only be turned down by so many dB. The input is delayed by
/// the stage's latency first, so the two line up.
pub struct ReductionCap {
    delay: Vec<f32>,
    at: usize,
    /// How much of the original stays: 10^(-dB/20), or 0 for unlimited.
    keep: f32,
}

/// At or above this the cap is off.
const UNLIMITED_DB: f32 = 100.0;

impl ReductionCap {
    pub fn new(latency_samples: usize) -> Self {
        Self {
            delay: vec![0.0; latency_samples.max(1)],
            at: 0,
            keep: 0.0,
        }
    }

    pub fn set_db(&mut self, db: f32) {
        self.keep = if db >= UNLIMITED_DB { 0.0 } else { 10f32.powf(-db.max(0.0) / 20.0) };
    }

    /// Replace each `processed` sample with the capped mix. `original` is what went in.
    pub fn apply(&mut self, original: &[f32], processed: &mut [f32]) {
        for (&dry, wet) in original.iter().zip(processed) {
            let old = std::mem::replace(&mut self.delay[self.at], dry);
            self.at = (self.at + 1) % self.delay.len();
            *wet = (1.0 - self.keep) * *wet + self.keep * old;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_leaves_the_processed_signal_alone() {
        let mut cap = ReductionCap::new(4);
        cap.set_db(100.0);
        let mut wet = [0.5; 8];
        cap.apply(&[1.0; 8], &mut wet);
        assert_eq!(wet, [0.5; 8]);
    }

    #[test]
    fn zero_db_gives_back_the_delayed_original() {
        let mut cap = ReductionCap::new(2);
        cap.set_db(0.0);
        let mut wet = [0.0; 5];
        cap.apply(&[1.0, 2.0, 3.0, 4.0, 5.0], &mut wet);
        assert_eq!(wet, [0.0, 0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn twenty_db_lets_a_tenth_of_the_original_through() {
        let mut cap = ReductionCap::new(1);
        cap.set_db(20.0);
        let mut wet = [0.0; 3];
        cap.apply(&[1.0; 3], &mut wet);
        assert!((wet[2] - 0.1).abs() < 1e-6, "{wet:?}");
    }
}
