//! RBJ-cookbook biquads and a Linkwitz-Riley 4th-order crossover.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

#[derive(Clone, Copy)]
pub struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalised(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
        }
    }

    fn trig(fs: f32, f: f32) -> (f64, f64) {
        // Keep the corner safely inside (0, Nyquist).
        let f = (f as f64).clamp(10.0, fs as f64 * 0.45);
        let w0 = 2.0 * PI * f / fs as f64;
        (w0.cos(), w0.sin())
    }

    pub fn lowpass(fs: f32, f: f32, q: f32) -> Self {
        let (cos, sin) = Self::trig(fs, f);
        let alpha = sin / (2.0 * q as f64);
        Self::normalised(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    pub fn highpass(fs: f32, f: f32, q: f32) -> Self {
        let (cos, sin) = Self::trig(fs, f);
        let alpha = sin / (2.0 * q as f64);
        Self::normalised(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    pub fn peaking(fs: f32, f: f32, q: f32, gain_db: f32) -> Self {
        let (cos, sin) = Self::trig(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let alpha = sin / (2.0 * q as f64);
        Self::normalised(
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        )
    }

    pub fn low_shelf(fs: f32, f: f32, gain_db: f32) -> Self {
        let (cos, sin) = Self::trig(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let beta = 2.0 * a.sqrt() * (sin * FRAC_1_SQRT_2);
        Self::normalised(
            [
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
            ],
            [
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            ],
        )
    }

    pub fn high_shelf(fs: f32, f: f32, gain_db: f32) -> Self {
        let (cos, sin) = Self::trig(fs, f);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let beta = 2.0 * a.sqrt() * (sin * FRAC_1_SQRT_2);
        Self::normalised(
            [
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
            ],
            [
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            ],
        )
    }
}

/// Transposed direct form II.
pub struct Biquad {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new(Coeffs::IDENTITY)
    }
}

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    /// Change the response without clearing state, so there is no click.
    pub fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    pub fn tick(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y as f32
    }
}

/// Linkwitz-Riley 4th order: low + high sum to flat magnitude.
#[derive(Default)]
pub struct Crossover {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    freq: f32,
}

impl Crossover {
    pub fn set_freq(&mut self, fs: f32, freq: f32) {
        if freq == self.freq {
            return;
        }
        self.freq = freq;
        let lp = Coeffs::lowpass(fs, freq, FRAC_1_SQRT_2 as f32);
        let hp = Coeffs::highpass(fs, freq, FRAC_1_SQRT_2 as f32);
        self.lp.iter_mut().for_each(|b| b.set(lp));
        self.hp.iter_mut().for_each(|b| b.set(hp));
    }

    /// Returns `(low, high)`.
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        let low = self.lp[0].tick(x);
        let low = self.lp[1].tick(low);
        let high = self.hp[0].tick(x);
        let high = self.hp[1].tick(high);
        (low, high)
    }

    /// Same phase response as `split`, magnitude 1. Used to time-align a band that skipped
    /// a later crossover.
    pub fn allpass(&mut self, x: f32) -> f32 {
        let (low, high) = self.split(x);
        low + high
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};

    const FS: f32 = 48_000.0;

    fn through(c: Coeffs, freq: f32) -> f32 {
        let mut b = Biquad::new(c);
        let input = sine(freq, 48_000, 0.5);
        let out: Vec<f32> = input.iter().map(|&x| b.tick(x)).collect();
        rms(&out[24_000..]) / rms(&input[24_000..])
    }

    #[test]
    fn highpass_cuts_rumble_keeps_voice() {
        let c = Coeffs::highpass(FS, 100.0, 0.707);
        assert!(through(c, 20.0) < 0.05);
        assert!((through(c, 1_000.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn peaking_boosts_by_requested_gain() {
        let c = Coeffs::peaking(FS, 1_000.0, 1.0, 6.0);
        let db = 20.0 * through(c, 1_000.0).log10();
        assert!((db - 6.0).abs() < 0.2, "got {db} dB");
    }

    #[test]
    fn shelves_boost_their_own_side() {
        let low = Coeffs::low_shelf(FS, 200.0, 6.0);
        assert!(through(low, 50.0) > 1.8 && (through(low, 5_000.0) - 1.0).abs() < 0.05);
        let high = Coeffs::high_shelf(FS, 6_000.0, 6.0);
        assert!(through(high, 15_000.0) > 1.8 && (through(high, 200.0) - 1.0).abs() < 0.05);
    }

    #[test]
    fn crossover_halves_sum_back_to_flat() {
        for freq in [150.0, 1_000.0, 6_000.0] {
            let mut x = Crossover::default();
            x.set_freq(FS, 1_000.0);
            let input = sine(freq, 48_000, 0.5);
            let out: Vec<f32> = input
                .iter()
                .map(|&s| {
                    let (l, h) = x.split(s);
                    l + h
                })
                .collect();
            let ratio = rms(&out[24_000..]) / rms(&input[24_000..]);
            assert!((ratio - 1.0).abs() < 0.03, "{freq} Hz ratio {ratio}");
        }
    }
}
