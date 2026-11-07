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

/// One band as the UI lays it out: which parameters it owns.
pub struct BandDef {
    pub label: &'static str,
    pub freq: usize,
    /// `None` for the high-pass, which has no gain.
    pub gain: Option<usize>,
}

pub const BANDS: [BandDef; 5] = [
    BandDef { label: "High-pass", freq: HPF, gain: None },
    BandDef { label: "Low shelf", freq: LOW_HZ, gain: Some(LOW_DB) },
    BandDef { label: "Low-mid", freq: LOW_MID_HZ, gain: Some(LOW_MID_DB) },
    BandDef { label: "High-mid", freq: HIGH_MID_HZ, gain: Some(HIGH_MID_DB) },
    BandDef { label: "High shelf", freq: HIGH_HZ, gain: Some(HIGH_DB) },
];

/// The five filters for these parameter values, in signal order.
fn filters(p: &[f32; COUNT], fs: f32) -> [Coeffs; 5] {
    [
        Coeffs::highpass(fs, p[HPF], FRAC_1_SQRT_2),
        Coeffs::low_shelf(fs, p[LOW_HZ], p[LOW_DB]),
        Coeffs::peaking(fs, p[LOW_MID_HZ], 1.0, p[LOW_MID_DB]),
        Coeffs::peaking(fs, p[HIGH_MID_HZ], 1.0, p[HIGH_MID_DB]),
        Coeffs::high_shelf(fs, p[HIGH_HZ], p[HIGH_DB]),
    ]
}

/// Combined EQ gain in dB at each of `freqs`, for the curve on screen.
pub fn response_db(section: &Section, fs: f32, freqs: &[f32]) -> Vec<f32> {
    let params: [f32; COUNT] = std::array::from_fn(|i| section.get(i));
    let filters = filters(&params, fs);
    freqs
        .iter()
        .map(|&f| filters.iter().map(|c| c.magnitude_db(fs, f)).sum())
        .collect()
}

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
        let [hp, low, low_mid, high_mid, high] = filters(&now, self.fs);
        self.hp.set(hp);
        self.low.set(low);
        self.low_mid.set(low_mid);
        self.high_mid.set(high_mid);
        self.high.set(high);
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
    fn drawn_curve_matches_the_real_audio() {
        let s = Arc::new(section());
        s.params[LOW_DB].set(5.0);
        s.params[HIGH_MID_DB].set(-7.0);
        s.params[HIGH_DB].set(4.0);
        let freqs = [60.0, 250.0, 1_000.0, 3_000.0, 9_000.0];
        let drawn = response_db(&s, 48_000.0, &freqs);
        for (&f, &predicted) in freqs.iter().zip(&drawn) {
            let mut eq = Eq::new(s.clone());
            eq.prepare(48_000);
            let mut tone = sine(f, 48_000, 0.05);
            eq.process(&mut tone);
            let measured = 20.0 * (rms(&tone[24_000..]) / (0.05 / 2f32.sqrt())).log10();
            assert!((measured - predicted).abs() < 0.3, "{f} Hz: heard {measured}, drew {predicted}");
        }
    }

    #[test]
    fn every_band_points_at_real_parameters() {
        let s = section();
        for band in &BANDS {
            assert!(s.params[band.freq].unit == "Hz", "{}", band.label);
            if let Some(g) = band.gain {
                assert!(s.params[g].unit == "dB", "{}", band.label);
            }
        }
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
