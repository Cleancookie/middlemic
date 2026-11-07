use super::Processor;
use super::biquad::{Biquad, Coeffs};
use crate::settings::{Param, Section};
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::Arc;

pub const NAME: &str = "EQ";

const HPF: usize = 0;
const LOW_HZ: usize = 1;
const LOW_DB: usize = 2;
const LOW_MID_HZ: usize = 3;
const LOW_MID_DB: usize = 4;
const HIGH_MID_HZ: usize = 5;
const HIGH_MID_DB: usize = 6;
const HIGH_HZ: usize = 7;
const HIGH_DB: usize = 8;
const COUNT: usize = 9;

pub fn section() -> Section {
    let gain = |name| Param::new(name, "dB", -12.0, 12.0, 0.0);
    Section::new(
        NAME,
        vec![
            Param::new("High-pass", "Hz", 20.0, 400.0, 80.0).log(),
            Param::new("Low shelf freq", "Hz", 60.0, 400.0, 120.0).log(),
            gain("Low shelf gain"),
            Param::new("Low-mid freq", "Hz", 150.0, 1500.0, 400.0).log(),
            gain("Low-mid gain"),
            Param::new("High-mid freq", "Hz", 1000.0, 8000.0, 3000.0).log(),
            gain("High-mid gain"),
            Param::new("High shelf freq", "Hz", 4000.0, 12000.0, 8000.0).log(),
            gain("High shelf gain"),
        ],
    )
}

/// High-pass, low shelf, two peaking bands, high shelf.
pub struct Eq {
    section: Arc<Section>,
    fs: f32,
    last: [f32; COUNT],
    hp: Biquad,
    low: Biquad,
    low_mid: Biquad,
    high_mid: Biquad,
    high: Biquad,
}

impl Eq {
    pub fn new(section: Arc<Section>) -> Self {
        Self {
            section,
            fs: 48_000.0,
            last: [f32::NAN; COUNT],
            hp: Biquad::default(),
            low: Biquad::default(),
            low_mid: Biquad::default(),
            high_mid: Biquad::default(),
            high: Biquad::default(),
        }
    }

    fn refresh(&mut self) {
        let now: [f32; COUNT] = std::array::from_fn(|i| self.section.get(i));
        if now == self.last {
            return;
        }
        self.last = now;
        let fs = self.fs;
        self.hp.set(Coeffs::highpass(fs, now[HPF], FRAC_1_SQRT_2));
        self.low.set(Coeffs::low_shelf(fs, now[LOW_HZ], now[LOW_DB]));
        self.low_mid
            .set(Coeffs::peaking(fs, now[LOW_MID_HZ], 1.0, now[LOW_MID_DB]));
        self.high_mid
            .set(Coeffs::peaking(fs, now[HIGH_MID_HZ], 1.0, now[HIGH_MID_DB]));
        self.high.set(Coeffs::high_shelf(fs, now[HIGH_HZ], now[HIGH_DB]));
    }
}

impl Processor for Eq {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        self.fs = sample_rate as f32;
        self.last = [f32::NAN; COUNT];
    }

    fn process(&mut self, samples: &mut [f32]) {
        self.refresh();
        for s in samples {
            let x = self.hp.tick(*s);
            let x = self.low.tick(x);
            let x = self.low_mid.tick(x);
            let x = self.high_mid.tick(x);
            *s = self.high.tick(x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};

    #[test]
    fn default_only_removes_rumble() {
        let mut eq = Eq::new(Arc::new(section()));
        eq.prepare(48_000);
        let mut voice = sine(1_000.0, 48_000, 0.5);
        let mut rumble = sine(25.0, 48_000, 0.5);
        eq.process(&mut voice);
        let mut eq = Eq::new(Arc::new(section()));
        eq.prepare(48_000);
        eq.process(&mut rumble);
        assert!((rms(&voice[24_000..]) / (0.5 / 2f32.sqrt()) - 1.0).abs() < 0.03);
        assert!(rms(&rumble[24_000..]) < 0.1 * (0.5 / 2f32.sqrt()));
    }

    #[test]
    fn parameter_change_takes_effect_live() {
        let s = Arc::new(section());
        let mut eq = Eq::new(s.clone());
        eq.prepare(48_000);
        s.params[HIGH_MID_DB].set(9.0);
        s.params[HIGH_MID_HZ].set(3_000.0);
        let mut tone = sine(3_000.0, 48_000, 0.1);
        eq.process(&mut tone);
        let gain_db = 20.0 * (rms(&tone[24_000..]) / (0.1 / 2f32.sqrt())).log10();
        assert!((gain_db - 9.0).abs() < 0.5, "got {gain_db} dB");
    }
}
