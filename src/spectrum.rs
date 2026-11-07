//! Turns the latest audio into something drawable: an FFT magnitude in dB, mapped onto
//! log-spaced columns, plus the frequency axis and colour map the displays share.

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::f32::consts::PI;
use std::sync::Arc;

pub const FFT_LEN: usize = 4096;
pub const MIN_HZ: f32 = 20.0;
pub const MAX_HZ: f32 = 20_000.0;
/// Quietest level drawn.
pub const FLOOR_DB: f32 = -100.0;

/// 0 at `MIN_HZ`, 1 at `MAX_HZ`, logarithmic.
pub fn freq_to_x(freq: f32) -> f32 {
    ((freq.max(MIN_HZ) / MIN_HZ).ln() / (MAX_HZ / MIN_HZ).ln()).clamp(0.0, 1.0)
}

pub fn x_to_freq(x: f32) -> f32 {
    MIN_HZ * (MAX_HZ / MIN_HZ).powf(x.clamp(0.0, 1.0))
}

pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    scale: f32,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Default for Analyzer {
    fn default() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let window: Vec<f32> = (0..FFT_LEN)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_LEN as f32).cos())
            .collect();
        Self {
            scale: 2.0 / window.iter().sum::<f32>(),
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            window,
            fft,
        }
    }
}

impl Analyzer {
    /// Level of each FFT bin in dBFS: a full-scale sine reads about 0 dB.
    /// `samples` must hold `FFT_LEN` values.
    pub fn analyze(&mut self, samples: &[f32]) -> Vec<f32> {
        self.power(samples).map(|p| (10.0 * p.max(1e-14).log10()).max(FLOOR_DB)).collect()
    }

    /// Linear power of each FFT bin: a full-scale sine reads about 1. Allocates nothing.
    pub fn power(&mut self, samples: &[f32]) -> impl Iterator<Item = f32> + '_ {
        for ((slot, s), w) in self.input.iter_mut().zip(samples).zip(&self.window) {
            *slot = s * w;
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .expect("buffers are sized by the planner");
        let scale = self.scale * self.scale;
        self.output.iter().map(move |c| c.norm_sqr() * scale)
    }
}

/// `points` log-spaced columns from `MIN_HZ` to `MAX_HZ`. Where a column spans several FFT
/// bins it takes the loudest, so narrow peaks are not lost; at low frequencies, where bins
/// are wider than columns, it interpolates.
pub fn to_columns(bins: &[f32], sample_rate: f32, points: usize) -> Vec<f32> {
    let bin_hz = sample_rate / FFT_LEN as f32;
    let last = bins.len() - 1;
    (0..points)
        .map(|i| {
            let lo = x_to_freq(i as f32 / points as f32) / bin_hz;
            let hi = x_to_freq((i + 1) as f32 / points as f32) / bin_hz;
            if hi - lo < 1.0 {
                let centre = (lo + hi) / 2.0;
                let a = centre.floor() as usize;
                let t = centre - a as f32;
                bins[a.min(last)] * (1.0 - t) + bins[(a + 1).min(last)] * t
            } else {
                bins[lo.floor() as usize..=(hi.ceil() as usize).min(last)]
                    .iter()
                    .copied()
                    .fold(FLOOR_DB, f32::max)
            }
        })
        .collect()
}

/// Colour for the spectrogram: black, through purple and orange, to pale yellow.
pub fn heat(db: f32) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [0.0, 0.0, 0.0]),
        (0.3, [40.0, 10.0, 90.0]),
        (0.6, [200.0, 40.0, 70.0]),
        (0.85, [255.0, 170.0, 40.0]),
        (1.0, [255.0, 250.0, 200.0]),
    ];
    let t = ((db + 90.0) / 70.0).clamp(0.0, 1.0);
    let hi = STOPS.iter().position(|(at, _)| *at >= t).unwrap_or(STOPS.len() - 1).max(1);
    let ((t0, c0), (t1, c1)) = (STOPS[hi - 1], STOPS[hi]);
    let f = (t - t0) / (t1 - t0);
    [0, 1, 2].map(|k| (c0[k] + (c1[k] - c0[k]) * f).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::sine;

    const FS: f32 = 48_000.0;
    /// A frequency that lands exactly on FFT bin 85, so there is no leakage to muddy the test.
    const BIN_85_HZ: f32 = 85.0 * FS / FFT_LEN as f32;

    #[test]
    fn axis_round_trips_and_hits_the_ends() {
        assert_eq!(freq_to_x(MIN_HZ), 0.0);
        assert!((freq_to_x(MAX_HZ) - 1.0).abs() < 1e-6);
        for f in [50.0, 440.0, 3_000.0, 12_000.0] {
            assert!((x_to_freq(freq_to_x(f)) - f).abs() / f < 1e-4);
        }
    }

    #[test]
    fn full_scale_style_sine_reads_its_true_level() {
        let mut a = Analyzer::default();
        let bins = a.analyze(&sine(BIN_85_HZ, FFT_LEN, 0.5));
        let (peak_bin, peak_db) = bins
            .iter()
            .enumerate()
            .fold((0, FLOOR_DB), |m, (i, &d)| if d > m.1 { (i, d) } else { m });
        assert_eq!(peak_bin, 85);
        assert!((peak_db - (-6.02)).abs() < 0.3, "{peak_db}");
        assert!(bins[300] < -60.0, "far-away bins are quiet: {}", bins[300]);
    }

    #[test]
    fn silence_sits_at_the_floor() {
        let mut a = Analyzer::default();
        assert!(a.analyze(&vec![0.0; FFT_LEN]).iter().all(|&d| d == FLOOR_DB));
    }

    #[test]
    fn columns_put_the_peak_where_the_frequency_is() {
        let mut a = Analyzer::default();
        let columns = to_columns(&a.analyze(&sine(BIN_85_HZ, FFT_LEN, 0.5)), FS, 240);
        let loudest = columns
            .iter()
            .enumerate()
            .fold((0, FLOOR_DB), |m, (i, &d)| if d > m.1 { (i, d) } else { m })
            .0;
        let expected = (freq_to_x(BIN_85_HZ) * 240.0) as usize;
        assert!(loudest.abs_diff(expected) <= 1, "column {loudest}, expected {expected}");
    }

    #[test]
    fn heat_gets_brighter_with_level() {
        let brightness = |db| heat(db).iter().map(|&c| c as u32).sum::<u32>();
        assert!(brightness(-100.0) < brightness(-70.0));
        assert!(brightness(-70.0) < brightness(-45.0));
        assert!(brightness(-45.0) < brightness(-10.0));
        assert_eq!(heat(-200.0), [0, 0, 0]);
    }
}
