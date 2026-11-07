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
        let high_xover = s.get(HIGH_XOVER);
        self.low_split.set_freq(self.fs, s.get(LOW_XOVER));
        self.high_split.set_freq(self.fs, high_xover);
        self.low_align.set_freq(self.fs, high_xover);

        let (attack, release) = (s.get(ATTACK), s.get(RELEASE));
        let mut threshold = [0.0; 3];
        let mut ratio = [1.0; 3];
        let mut makeup = [1.0; 3];
        for band in 0..3 {
            let at = BANDS_AT + band * PER_BAND;
            threshold[band] = s.get(at);
            ratio[band] = s.get(at + 1);
            makeup[band] = 10f32.powf(s.get(at + 2) / 20.0);
            self.comps[band].set_times(self.fs, attack, release);
        }

        for sample in samples {
            let (low, rest) = self.low_split.split(*sample);
            let (mid, high) = self.high_split.split(rest);
            let low = self.low_align.allpass(low);
            let bands = [low, mid, high];
            let mut out = 0.0;
            for (band, x) in bands.into_iter().enumerate() {
                let g = self.comps[band].gain(x, threshold[band], ratio[band], 60.0);
                out += x * g * makeup[band];
            }
            *sample = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};

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
            let r = run(freq, 0.01, section());
            assert!((r - 1.0).abs() < 0.05, "{freq} Hz: {r}");
        }
    }

    #[test]
    fn loud_signal_is_compressed_in_its_own_band() {
        for freq in [100.0, 1_000.0, 6_000.0] {
            let r = run(freq, 0.5, section());
            assert!(r < 0.85, "{freq} Hz was not compressed: {r}");
        }
    }

    #[test]
    fn makeup_gain_raises_the_band() {
        let s = section();
        s.params[BANDS_AT + PER_BAND + 2].set(6.0); // mid makeup
        let r = run(1_000.0, 0.01, s);
        assert!(r > 1.8, "{r}");
    }
}
