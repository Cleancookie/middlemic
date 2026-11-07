use super::Processor;
use super::biquad::Crossover;
use super::compressor::GainComputer;
use crate::settings::{Param, Section};
use std::sync::Arc;

pub const NAME: &str = "Compressor";

const LOW_XOVER: usize = 0;
const HIGH_XOVER: usize = 1;
const ATTACK: usize = 2;
const RELEASE: usize = 3;
/// Per band: threshold, ratio, makeup, starting here.
const BANDS_AT: usize = 4;
const PER_BAND: usize = 3;

pub fn section() -> Section {
    let band = |t, r, m, ratio: f32| {
        [
            Param::new(t, "dB", -60.0, 0.0, -24.0),
            Param::new(r, ":1", 1.0, 10.0, ratio),
            Param::new(m, "dB", 0.0, 18.0, 0.0),
        ]
    };
    let mut params = vec![
        Param::new("Low crossover", "Hz", 60.0, 800.0, 200.0).log(),
        Param::new("High crossover", "Hz", 1000.0, 8000.0, 3000.0).log(),
        Param::new("Attack", "ms", 1.0, 100.0, 10.0),
        Param::new("Release", "ms", 20.0, 500.0, 120.0),
    ];
    params.extend(band("Low threshold", "Low ratio", "Low makeup", 2.5));
    params.extend(band("Mid threshold", "Mid ratio", "Mid makeup", 3.0));
    params.extend(band("High threshold", "High ratio", "High makeup", 2.0));
    Section::new(NAME, params)
        .with_reduction_meters(&["Low", "Mid", "High"])
        .with_simple(
            vec![Param::new("Amount", "%", 0.0, 100.0, 50.0)],
            &[("Gentle", 25.0), ("Voice", 50.0), ("Strong", 80.0)],
        )
}

#[derive(Clone, Copy)]
struct Band {
    threshold: f32,
    ratio: f32,
    makeup_db: f32,
}

/// One "Amount" (0..1) becomes all three bands. The low and high bands work a little
/// less hard than the mid, where most of the voice is. Make-up gain rises with the amount
/// so turning it up does not make the voice quieter.
fn simple_bands(amount: f32) -> [Band; 3] {
    let a = amount.clamp(0.0, 1.0);
    let lerp = |from: f32, to: f32| from + (to - from) * a;
    let edge = Band {
        threshold: lerp(-8.0, -34.0),
        ratio: lerp(1.3, 3.0),
        makeup_db: lerp(0.0, 4.0),
    };
    let mid = Band {
        threshold: lerp(-6.0, -36.0),
        ratio: lerp(1.5, 4.0),
        makeup_db: lerp(0.0, 6.0),
    };
    [edge, mid, edge]
}

/// Three-band compressor. Bands are split with Linkwitz-Riley crossovers and the low band is
/// all-passed through the upper crossover so the bands recombine without phase cancellation.
pub struct Multiband {
    section: Arc<Section>,
    fs: f32,
    low_split: Crossover,
    high_split: Crossover,
    low_align: Crossover,
    comps: [GainComputer; 3],
}

impl Multiband {
    pub fn new(section: Arc<Section>) -> Self {
        Self {
            section,
            fs: 48_000.0,
            low_split: Crossover::default(),
            high_split: Crossover::default(),
            low_align: Crossover::default(),
            comps: Default::default(),
        }
    }
}

impl Processor for Multiband {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        *self = Self {
            fs: sample_rate as f32,
            ..Self::new(self.section.clone())
        };
    }

    fn process(&mut self, samples: &mut [f32]) {
        let s = &self.section;
        let advanced = s.is_advanced();
        // Simple mode keeps the crossovers and times at their defaults.
        let value = |i: usize| if advanced { s.get(i) } else { s.params[i].default };
        let high_xover = value(HIGH_XOVER);
        self.low_split.set_freq(self.fs, value(LOW_XOVER));
        self.high_split.set_freq(self.fs, high_xover);
        self.low_align.set_freq(self.fs, high_xover);

        let bands: [Band; 3] = if advanced {
            std::array::from_fn(|band| {
                let at = BANDS_AT + band * PER_BAND;
                Band {
                    threshold: s.get(at),
                    ratio: s.get(at + 1),
                    makeup_db: s.get(at + 2),
                }
            })
        } else {
            simple_bands(s.simple[0].get() / 100.0)
        };
        let (attack, release) = (value(ATTACK), value(RELEASE));
        let mut threshold = [0.0; 3];
        let mut ratio = [1.0; 3];
        let mut makeup = [1.0; 3];
        for (band, b) in bands.iter().enumerate() {
            threshold[band] = b.threshold;
            ratio[band] = b.ratio;
            makeup[band] = 10f32.powf(b.makeup_db / 20.0);
            self.comps[band].set_times(self.fs, attack, release);
        }

        let mut worst = [0f32; 3];
        for sample in samples {
            let (low, rest) = self.low_split.split(*sample);
            let (mid, high) = self.high_split.split(rest);
            let low = self.low_align.allpass(low);
            let bands = [low, mid, high];
            let mut out = 0.0;
            for (band, x) in bands.into_iter().enumerate() {
                let g = self.comps[band].gain(x, threshold[band], ratio[band], 60.0);
                out += x * g * makeup[band];
                worst[band] = worst[band].max(self.comps[band].reduction_db());
            }
            *sample = out;
        }
        for (band, db) in worst.into_iter().enumerate() {
            self.section.record_reduction(band, db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};

    fn advanced() -> Section {
        let s = section();
        s.advanced.store(true, std::sync::atomic::Ordering::Relaxed);
        s
    }

    fn simple_at(amount: f32) -> Section {
        let s = section();
        s.simple[0].set(amount);
        s
    }

    fn run(freq: f32, amp: f32, section: Section) -> f32 {
        let mut m = Multiband::new(Arc::new(section));
        m.prepare(48_000);
        let mut tone = sine(freq, 96_000, amp);
        m.process(&mut tone);
        rms(&tone[72_000..]) / rms(&sine(freq, 96_000, amp)[72_000..])
    }

    #[test]
    fn quiet_signal_passes_flat() {
        for freq in [100.0, 1_000.0, 6_000.0] {
            let r = run(freq, 0.01, advanced());
            assert!((r - 1.0).abs() < 0.05, "{freq} Hz: {r}");
        }
    }

    #[test]
    fn loud_signal_is_compressed_in_its_own_band() {
        for freq in [100.0, 1_000.0, 6_000.0] {
            let r = run(freq, 0.5, advanced());
            assert!(r < 0.85, "{freq} Hz was not compressed: {r}");
        }
    }

    #[test]
    fn meters_show_which_band_is_working() {
        let s = Arc::new(advanced());
        let mut m = Multiband::new(s.clone());
        m.prepare(48_000);
        m.process(&mut sine(1_000.0, 48_000, 0.5));
        let (low, mid, high) = (s.take_reduction(0), s.take_reduction(1), s.take_reduction(2));
        assert!(mid > 3.0, "mid {mid}");
        assert!(low < 1.0 && high < 1.0, "low {low} high {high}");
    }

    #[test]
    fn simple_mode_is_the_default_and_compresses() {
        assert!(!section().is_advanced());
        let r = run(1_000.0, 0.5, section());
        assert!(r < 0.9, "default Amount did not compress a loud tone: {r}");
    }

    #[test]
    fn higher_amount_compresses_harder() {
        // Make-up gain grows with Amount, so compare the meter, not the output level.
        let reduction_db = |amount: f32| {
            let s = Arc::new(simple_at(amount));
            let mut m = Multiband::new(s.clone());
            m.prepare(48_000);
            m.process(&mut sine(1_000.0, 96_000, 0.5));
            s.take_reduction(1)
        };
        assert!(reduction_db(0.0) < 1.0);
        assert!(reduction_db(50.0) > reduction_db(0.0) + 3.0);
        assert!(reduction_db(100.0) > reduction_db(50.0) + 3.0);
    }

    #[test]
    fn simple_quiet_voice_is_not_squashed_flat() {
        // Quiet speech sits under the threshold, so only make-up gain applies (at most +6 dB).
        let r = run(1_000.0, 0.01, simple_at(100.0));
        assert!(r > 1.0 && r < 2.1, "{r}");
    }

    #[test]
    fn makeup_gain_raises_the_band() {
        let s = advanced();
        s.params[BANDS_AT + PER_BAND + 2].set(6.0); // mid makeup
        let r = run(1_000.0, 0.01, s);
        assert!(r > 1.8, "{r}");
    }
}
