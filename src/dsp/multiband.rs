use super::Processor;
use super::biquad::Crossover;
use super::compressor::{Curve, Envelope, db_to_gain};
use crate::settings::{Param, Section};
use std::sync::Arc;

pub const NAME: &str = "Compressor";

pub const LOW_XOVER: usize = 0;
pub const HIGH_XOVER: usize = 1;
pub const LOOKAHEAD: usize = 2;
const STRIPS_AT: usize = 3;
const MAX_LOOKAHEAD_MS: f32 = 5.0;
const RMS_WINDOW_MS: f32 = 10.0;
const MAX_REDUCTION_DB: f32 = 60.0;

/// The three bands, then the master that follows their sum. Also the reduction meter slots;
/// level meter `2 * strip` is a strip's input and `2 * strip + 1` its output.
pub const STRIPS: [&str; 4] = ["Low", "Mid", "High", "Master"];
pub const MASTER: usize = 3;

/// What each strip is for, one line. Also appended to the strips' threshold tooltips.
macro_rules! role {
    ("Low") => { "Low band: rumble, desk thumps, plosives (p and b pops) and boom." };
    ("Mid") => { "Mid band: the body of the voice, where most words live." };
    ("High") => { "High band: sibilance (harsh s sounds), hiss and air." };
    ("Master") => { "Master: the whole voice after the three bands are added back together." };
}
pub const BAND_HELP: [&str; 4] = [role!("Low"), role!("Mid"), role!("High"), role!("Master")];
pub const BAND_TITLES: [&str; 4] = ["Low band", "Mid band", "High band", "Master compressor"];

/// Where each control sits within a strip. The master stops before the switches.
pub mod field {
    pub const INPUT: usize = 0;
    pub const OUTPUT: usize = 1;
    pub const THRESHOLD: usize = 2;
    pub const RATIO: usize = 3;
    pub const KNEE: usize = 4;
    pub const ATTACK: usize = 5;
    pub const HOLD: usize = 6;
    pub const RELEASE: usize = 7;
    /// 0 peak, 1 RMS.
    pub const RMS: usize = 8;
    pub const FLOOR: usize = 9;
    pub const FLOOR_RATIO: usize = 10;
    pub const SOLO: usize = 11;
    pub const MUTE: usize = 12;
    pub const BYPASS: usize = 13;
}
const BAND_FIELDS: usize = 14;

/// Index into the section's params of `field` of `strip`.
pub fn param(strip: usize, field: usize) -> usize {
    STRIPS_AT + strip * BAND_FIELDS + field
}

macro_rules! strip {
    ($name:tt, $threshold:expr, $ratio:expr) => {
        [
            Param::new(concat!($name, " input"), "dB", -24.0, 24.0, 0.0).help(
                "Volume change applied before compressing, in dB (decibels, a loudness unit). Start at 0. \
                 Raising it pushes more sound over the threshold, so it is compressed harder; lowering it eases off.",
            ),
            Param::new(concat!($name, " output"), "dB", -24.0, 24.0, 0.0).help(
                "Volume change applied after compressing, in dB. Start at 0. \
                 Raise it 1 to 4 dB if the voice sounds quieter after compression, to make up what was turned down.",
            ),
            Param::new(concat!($name, " threshold"), "dB", -60.0, 0.0, $threshold).help(concat!(
                "The level above which sound is turned down, in dB (0 is the loudest possible). Lower means more gets compressed. \
                 Start near the default and watch the reduction meter: 3 to 6 dB on normal speech is plenty. ",
                role!($name)
            )),
            Param::new(concat!($name, " ratio"), ":1", 1.0, 20.0, $ratio).log().help(
                "How hard sound above the threshold is squashed. At 2:1, every 2 dB over the threshold becomes 1 dB. \
                 Start around 2:1 to 3:1. Higher sounds heavier and can flatten your voice; 1:1 is off.",
            ),
            Param::new(concat!($name, " knee"), "dB", 0.0, 24.0, 0.0).help(
                "How gradually compression starts around the threshold, in dB. 0 switches on abruptly; more blends in smoothly. \
                 Try 6 dB for a gentler, less obvious effect on voice.",
            ),
            Param::new(concat!($name, " attack"), "ms", 0.0, 100.0, 10.0).help(
                "How fast it clamps down on loud sound, in ms (thousandths of a second). \
                 Short catches sudden peaks but can dull the start of words; long lets the punch through. Start at 10 ms.",
            ),
            Param::new(concat!($name, " hold"), "ms", 0.0, 500.0, 0.0).help(
                "How long it stays turned down after the sound drops, in ms, before letting go. Start at 0. \
                 Try 20 to 50 ms if it flutters on low voices or rapid syllables.",
            ),
            Param::new(concat!($name, " release"), "ms", 10.0, 1000.0, 120.0).log().help(
                "How fast the volume comes back after a loud sound ends, in ms. \
                 Short sounds lively but can pump; long is smooth but may duck the next word. Start at 120 ms; 100 to 250 suits speech.",
            ),
            Param::new(concat!($name, " RMS"), "", 0.0, 1.0, 0.0).help(
                "How loudness is measured. Peak catches the loudest instant, so quick spikes are caught. \
                 RMS averages over about 10 ms, which is smoother and closer to how you hear. RMS is a good start for voice.",
            ),
            Param::new(concat!($name, " floor"), "dB", -90.0, 0.0, -60.0).help(
                "The quiet level, in dB, under which sound is turned down further, like a soft gate. \
                 Raise it until fan or keyboard noise between words drops away; too high cuts quiet word endings. \
                 Does nothing while Floor ratio is 1:1.",
            ),
            Param::new(concat!($name, " floor ratio"), "1:", 1.0, 4.0, 1.0).help(
                "How strongly sound below the floor is pushed down. 1:1 is off; at 1:2 every dB under the floor falls another dB. \
                 Start around 1:2. Raise it if keyboard or fan noise between words still comes through; too high cuts off quiet word endings.",
            ),
        ]
    };
}

macro_rules! switches {
    ($name:tt) => {
        [
            Param::new(concat!($name, " solo"), "", 0.0, 1.0, 0.0).help(concat!(
                "Plays only the soloed bands, so you can hear what one range of your voice is doing. \
                 Turn it off before going live or everyone hears just that band. ",
                role!($name)
            )),
            Param::new(concat!($name, " mute"), "", 0.0, 1.0, 0.0).help(concat!(
                "Silences this band completely. Good for checking what it contributes; turn it off before going live. ",
                role!($name)
            )),
            Param::new(concat!($name, " bypass"), "", 0.0, 1.0, 0.0).help(concat!(
                "Lets this band through uncompressed and with no gain changes, to compare with and without. ",
                role!($name)
            )),
        ]
    };
}

pub fn section() -> Section {
    let mut params = vec![
        Param::new("Low crossover", "Hz", 60.0, 800.0, 200.0).log().help(
            "Where the Low band ends and the Mid band begins, in Hz (pitch: low numbers are bass). \
             Start at 200 Hz. Drag the left divider on the graph to move it.",
        ),
        Param::new("High crossover", "Hz", 1000.0, 8000.0, 3000.0).log().help(
            "Where the Mid band ends and the High band begins, in Hz. \
             Start at 3000 Hz. Drag the right divider on the graph to move it.",
        ),
        Param::new("Lookahead", "ms", 0.0, MAX_LOOKAHEAD_MS, 0.0).help(
            "Delays your voice slightly so the compressor sees a loud sound before it plays and can catch it from its first instant. \
             0 is off. Every ms adds that much delay to your voice. Try 1 to 2 ms if the first syllable of a shout gets through.",
        ),
    ];
    params.extend(strip!("Low", -24.0, 2.5));
    params.extend(switches!("Low"));
    params.extend(strip!("Mid", -24.0, 3.0));
    params.extend(switches!("Mid"));
    params.extend(strip!("High", -24.0, 2.0));
    params.extend(switches!("High"));
    params.extend(strip!("Master", -12.0, 1.0));
    Section::new(NAME, params)
        .help(
            "Evens out your volume by turning down the loud parts, so whispers and shouts sit closer together. \
             Advanced splits your voice into Low, Mid and High bands, compresses each, then a Master compressor works on the sum.",
        )
        .with_reduction_meters(&STRIPS)
        .with_level_meters(STRIPS.len() * 2)
        .with_renamed(&[
            ("Attack", &["Low attack", "Mid attack", "High attack"]),
            ("Release", &["Low release", "Mid release", "High release"]),
            ("Low makeup", &["Low output"]),
            ("Mid makeup", &["Mid output"]),
            ("High makeup", &["High output"]),
        ])
        .with_simple(
            vec![Param::new("Amount", "%", 0.0, 100.0, 50.0).help(
                "How much to even out your volume, in one control: higher turns loud parts down harder, \
                 and adds make-up volume so you do not get quieter. Start at 50. Tick Advanced for the full controls.",
            )],
            &[
                ("Gentle", 25.0, "A light touch that keeps your natural dynamics. Sets Amount to 25%."),
                ("Voice", 50.0, "A balanced everyday setting for chat and calls. Sets Amount to 50%."),
                (
                    "Strong",
                    80.0,
                    "Heavy levelling for loud games or a very dynamic voice. Can sound squashed and brings up background noise. Sets Amount to 80%.",
                ),
            ],
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

/// One strip's settings for a block, gains linear.
#[derive(Clone, Copy)]
struct Strip {
    input: f32,
    output: f32,
    curve: Curve,
    attack: f32,
    hold: f32,
    release: f32,
    rms: bool,
    solo: bool,
    mute: bool,
    bypass: bool,
}

fn curve_with(strip: usize, value: &impl Fn(usize) -> f32) -> Curve {
    let f = |field| value(param(strip, field));
    Curve {
        threshold: f(field::THRESHOLD),
        ratio: f(field::RATIO),
        knee: f(field::KNEE),
        floor: f(field::FLOOR),
        floor_ratio: f(field::FLOOR_RATIO),
    }
}

/// The static curve `strip`'s controls set, as the Advanced view shows it.
pub fn curve(section: &Section, strip: usize) -> Curve {
    curve_with(strip, &|i| section.get(i))
}

impl Strip {
    fn read(strip: usize, value: &impl Fn(usize) -> f32) -> Self {
        let f = |field| value(param(strip, field));
        let switch = |field| strip != MASTER && f(field) >= 0.5;
        Self {
            input: db_to_gain(f(field::INPUT)),
            output: db_to_gain(f(field::OUTPUT)),
            curve: curve_with(strip, value),
            attack: f(field::ATTACK),
            hold: f(field::HOLD),
            release: f(field::RELEASE),
            rms: f(field::RMS) >= 0.5,
            solo: switch(field::SOLO),
            mute: switch(field::MUTE),
            bypass: switch(field::BYPASS),
        }
    }
}

/// A strip's detector and gain smoothing.
#[derive(Default)]
struct Dynamics {
    compress: Envelope,
    /// Holds the negated expansion, so the floor opens on the attack and closes on the release.
    expand: Envelope,
    mean_square: f32,
    rms_coeff: f32,
}

impl Dynamics {
    fn configure(&mut self, fs: f32, s: &Strip) {
        self.compress.set_times(fs, s.attack, s.hold, s.release);
        self.expand.set_times(fs, s.attack, s.hold, s.release);
        self.rms_coeff = (-1000.0 / (RMS_WINDOW_MS * fs)).exp();
        if s.curve.is_flat() {
            self.compress.clear();
            self.expand.clear();
        }
    }

    /// Gain reduction (dB, positive) for this sample of the detector signal.
    fn reduction_db(&mut self, x: f32, s: &Strip) -> f32 {
        if s.curve.is_flat() {
            return 0.0;
        }
        let level_db = if s.rms {
            self.mean_square = x * x + (self.mean_square - x * x) * self.rms_coeff;
            10.0 * self.mean_square.max(1e-12).log10()
        } else {
            20.0 * x.abs().max(1e-6).log10()
        };
        let compress = self.compress.tick(s.curve.compression_db(level_db).min(MAX_REDUCTION_DB));
        let expand = -self.expand.tick(-s.curve.expansion_db(level_db).min(MAX_REDUCTION_DB));
        (compress + expand).min(MAX_REDUCTION_DB)
    }
}

fn reduction_gain(db: f32) -> f32 {
    if db > 0.0 { db_to_gain(-db) } else { 1.0 }
}

/// A fixed-size delay line; reads up to its length minus one samples back.
#[derive(Default)]
struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    fn new(max: usize) -> Self {
        Self { buf: vec![0.0; max + 1], pos: 0 }
    }

    fn tick(&mut self, x: f32, samples: usize) -> f32 {
        let len = self.buf.len();
        self.buf[self.pos] = x;
        let out = self.buf[(self.pos + len - samples.min(len - 1)) % len];
        self.pos = (self.pos + 1) % len;
        out
    }
}

/// The loudest values seen in a block, for one strip's meters.
#[derive(Clone, Copy, Default)]
struct Peaks {
    reduction: f32,
    input: f32,
    output: f32,
}

impl Peaks {
    fn take(&mut self, reduction: f32, input: f32, output: f32) {
        self.reduction = self.reduction.max(reduction);
        self.input = self.input.max(input.abs());
        self.output = self.output.max(output.abs());
    }
}

/// Three-band compressor with a master compressor on the sum, each strip with gain staging,
/// soft knee, hold, peak or RMS detection and a downward expander under a floor. Bands are
/// split with Linkwitz-Riley crossovers and the low band is all-passed through the upper
/// crossover so the bands recombine without phase cancellation. Lookahead delays the audio
/// while the detectors hear it undelayed.
pub struct Multiband {
    section: Arc<Section>,
    fs: f32,
    low_split: Crossover,
    high_split: Crossover,
    low_align: Crossover,
    dynamics: [Dynamics; 4],
    delays: [Delay; 3],
}

impl Multiband {
    pub fn new(section: Arc<Section>) -> Self {
        let mut stage = Self {
            section,
            fs: 48_000.0,
            low_split: Crossover::default(),
            high_split: Crossover::default(),
            low_align: Crossover::default(),
            dynamics: Default::default(),
            delays: Default::default(),
        };
        stage.prepare(48_000);
        stage
    }

    fn lookahead_samples(&self, ms: f32) -> usize {
        (ms.clamp(0.0, MAX_LOOKAHEAD_MS) * 0.001 * self.fs).round() as usize
    }

    /// Simple mode keeps everything but its few dials at the defaults.
    fn value(&self, index: usize) -> f32 {
        let s = &self.section;
        if s.is_advanced() { s.get(index) } else { s.params[index].default }
    }
}

impl Processor for Multiband {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        self.fs = sample_rate as f32;
        let max = self.lookahead_samples(MAX_LOOKAHEAD_MS);
        *self = Self {
            section: self.section.clone(),
            fs: self.fs,
            low_split: Crossover::default(),
            high_split: Crossover::default(),
            low_align: Crossover::default(),
            dynamics: Default::default(),
            delays: std::array::from_fn(|_| Delay::new(max)),
        };
    }

    fn latency_samples(&self) -> usize {
        self.lookahead_samples(self.value(LOOKAHEAD))
    }

    fn process(&mut self, samples: &mut [f32]) {
        let section = Arc::clone(&self.section);
        let value = |i: usize| self.value(i);
        let high_xover = value(HIGH_XOVER);
        let (low_xover, delay) = (value(LOW_XOVER), self.latency_samples());
        let mut strips: [Strip; 4] = std::array::from_fn(|k| Strip::read(k, &value));
        if !section.is_advanced() {
            for (strip, band) in strips.iter_mut().zip(simple_bands(section.simple[0].get() / 100.0)) {
                strip.curve.threshold = band.threshold;
                strip.curve.ratio = band.ratio;
                strip.output = db_to_gain(band.makeup_db);
            }
        }
        self.low_split.set_freq(self.fs, low_xover);
        self.high_split.set_freq(self.fs, high_xover);
        self.low_align.set_freq(self.fs, high_xover);
        for (dynamics, strip) in self.dynamics.iter_mut().zip(&strips) {
            dynamics.configure(self.fs, strip);
        }
        let soloing = strips[..MASTER].iter().any(|s| s.solo);
        let audible: [bool; 3] = std::array::from_fn(|b| !strips[b].mute && (strips[b].solo || !soloing));

        let mut peaks = [Peaks::default(); 4];
        for sample in samples {
            let (low, rest) = self.low_split.split(*sample);
            let (mid, high) = self.high_split.split(rest);
            let bands = [self.low_align.allpass(low), mid, high];
            // `ahead` is what the master's detector hears: the bands before the lookahead delay.
            let (mut sum, mut ahead) = (0.0, 0.0);
            for (b, x) in bands.into_iter().enumerate() {
                let s = &strips[b];
                let late = self.delays[b].tick(x, delay);
                let (reduction, gain) = if s.bypass {
                    (0.0, 1.0)
                } else {
                    let db = self.dynamics[b].reduction_db(x * s.input, s);
                    (db, s.input * reduction_gain(db) * s.output)
                };
                peaks[b].take(reduction, x * s.input, late * gain);
                if audible[b] {
                    sum += late * gain;
                    ahead += x * gain;
                }
            }
            let m = &strips[MASTER];
            let db = self.dynamics[MASTER].reduction_db(ahead * m.input, m);
            let out = sum * m.input * reduction_gain(db) * m.output;
            peaks[MASTER].take(db, ahead * m.input, out);
            *sample = out;
        }
        for (strip, p) in peaks.iter().enumerate() {
            section.record_reduction(strip, p.reduction);
            section.record_level(2 * strip, p.input);
            section.record_level(2 * strip + 1, p.output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};
    use crate::settings::Settings;

    fn advanced() -> Section {
        let s = section();
        s.advanced.store(true, std::sync::atomic::Ordering::Relaxed);
        s
    }

    /// Advanced, with every strip set to leave the level alone.
    fn neutral() -> Section {
        let s = advanced();
        for strip in 0..STRIPS.len() {
            s.params[param(strip, field::RATIO)].set(1.0);
        }
        s
    }

    fn set(s: &Section, strip: usize, field: usize, value: f32) {
        s.params[param(strip, field)].set(value);
    }

    fn simple_at(amount: f32) -> Section {
        let s = section();
        s.simple[0].set(amount);
        s
    }

    /// Output over input level of a steady tone, after it settles.
    fn run(freq: f32, amp: f32, section: Section) -> f32 {
        let mut m = Multiband::new(Arc::new(section));
        let mut tone = sine(freq, 96_000, amp);
        m.process(&mut tone);
        rms(&tone[72_000..]) / rms(&sine(freq, 96_000, amp)[72_000..])
    }

    fn db(ratio: f32) -> f32 {
        20.0 * ratio.log10()
    }

    #[test]
    fn neutral_bands_sum_back_flat() {
        for freq in [40.0, 150.0, 200.0, 700.0, 3_000.0, 8_000.0, 15_000.0] {
            let gain = db(run(freq, 0.5, neutral()));
            assert!(gain.abs() < 0.1, "{freq} Hz: {gain:.3} dB");
        }
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

    /// A 1 kHz sine of amplitude 0.5 reads -9.03 dB RMS.
    const TONE_RMS_DB: f32 = -9.03;

    /// Neutral, with the crossovers pushed out so a 1 kHz tone is all in the mid band.
    fn wide() -> Section {
        let s = neutral();
        s.params[LOW_XOVER].set(60.0);
        s.params[HIGH_XOVER].set(8_000.0);
        s
    }

    fn mid_rms(threshold: f32, ratio: f32, knee: f32) -> Section {
        let s = wide();
        set(&s, 1, field::RMS, 1.0);
        set(&s, 1, field::THRESHOLD, threshold);
        set(&s, 1, field::RATIO, ratio);
        set(&s, 1, field::KNEE, knee);
        s
    }

    #[test]
    fn steady_tone_settles_on_the_static_curve() {
        let hard = db(run(1_000.0, 0.5, mid_rms(TONE_RMS_DB - 20.0, 4.0, 0.0)));
        assert!((hard + 15.0).abs() < 0.3, "20 dB over at 4:1: {hard:.2} dB");
        let soft = db(run(1_000.0, 0.5, mid_rms(TONE_RMS_DB, 4.0, 12.0)));
        assert!((soft + 1.125).abs() < 0.2, "at the threshold inside a 12 dB knee: {soft:.2} dB");
        let steep = db(run(1_000.0, 0.5, mid_rms(TONE_RMS_DB - 20.0, 20.0, 0.0)));
        assert!((steep + 19.0).abs() < 0.3, "20:1: {steep:.2} dB");
    }

    #[test]
    fn the_floor_expands_quiet_sound_and_leaves_louder_sound_alone() {
        let floor = |s: &Section| {
            set(s, 1, field::RMS, 1.0);
            set(s, 1, field::FLOOR, -40.0);
            set(s, 1, field::FLOOR_RATIO, 3.0);
        };
        let (quiet, loud) = (wide(), wide());
        floor(&quiet);
        floor(&loud);
        // 0.00447 reads -50 dB RMS: 10 dB under the floor at 1:3 is turned down 20 dB.
        let under = db(run(1_000.0, 0.00447, quiet));
        assert!((under + 20.0).abs() < 0.5, "{under:.2} dB");
        let over = db(run(1_000.0, 0.1, loud));
        assert!(over.abs() < 0.1, "{over:.2} dB");
    }

    fn dynamics(attack: f32, hold: f32, release: f32) -> (Dynamics, Strip) {
        let s = neutral();
        set(&s, 1, field::THRESHOLD, -40.0);
        set(&s, 1, field::RATIO, 2.0);
        set(&s, 1, field::ATTACK, attack);
        set(&s, 1, field::HOLD, hold);
        set(&s, 1, field::RELEASE, release);
        let strip = Strip::read(1, &|i: usize| s.get(i));
        let mut d = Dynamics::default();
        d.configure(48_000.0, &strip);
        (d, strip)
    }

    #[test]
    fn attack_hold_and_release_take_their_times() {
        let ms = |n: f32| (n * 48.0) as usize;
        let (mut d, strip) = dynamics(10.0, 50.0, 100.0);
        // 0 dBFS is 40 dB over at 2:1: 20 dB of reduction once settled.
        let mut last = 0.0;
        for _ in 0..ms(10.0) {
            last = d.reduction_db(1.0, &strip);
        }
        assert!((last - 20.0 * (1.0 - (-1f32).exp())).abs() < 0.3, "one attack time reaches 63 %: {last}");
        for _ in 0..ms(200.0) {
            last = d.reduction_db(1.0, &strip);
        }
        assert!((last - 20.0).abs() < 0.01);
        let settled = last;
        for _ in 0..ms(49.0) {
            last = d.reduction_db(1e-4, &strip);
        }
        assert_eq!(last, settled, "held");
        for _ in 0..ms(101.0) {
            last = d.reduction_db(1e-4, &strip);
        }
        assert!((last - 20.0 * (-1f32).exp()).abs() < 0.3, "one release time later 37 % is left: {last}");
    }

    #[test]
    fn mute_solo_and_bypass_pick_what_is_heard() {
        let muted = neutral();
        set(&muted, 1, field::MUTE, 1.0);
        assert!(db(run(1_000.0, 0.5, muted)) < -25.0, "muted mid");

        let solo_low = neutral();
        set(&solo_low, 0, field::SOLO, 1.0);
        assert!(db(run(1_000.0, 0.5, solo_low)) < -25.0, "soloing low silences the mid");
        let solo_low = neutral();
        set(&solo_low, 0, field::SOLO, 1.0);
        assert!(db(run(60.0, 0.5, solo_low)).abs() < 0.3, "and keeps the low");

        let squash = |bypass: f32| {
            let s = mid_rms(-60.0, 20.0, 0.0);
            set(&s, 1, field::BYPASS, bypass);
            db(run(1_000.0, 0.5, s))
        };
        assert!(squash(0.0) < -20.0);
        assert!(squash(1.0).abs() < 0.3, "bypass passes the band untouched");
    }

    #[test]
    fn the_master_compresses_the_sum() {
        let s = Arc::new(neutral());
        set(&s, MASTER, field::RATIO, 4.0);
        set(&s, MASTER, field::THRESHOLD, -30.0);
        set(&s, MASTER, field::OUTPUT, 3.0);
        let mut m = Multiband::new(s.clone());
        m.process(&mut sine(1_000.0, 48_000, 0.5));
        assert!(s.take_reduction(MASTER) > 15.0);
        assert!(s.take_level(2 * MASTER) > 0.45, "master input meter");
    }

    #[test]
    fn lookahead_delays_the_audio_and_reports_it() {
        let peak_at = |lookahead: f32| {
            let s = neutral();
            s.params[LOOKAHEAD].set(lookahead);
            let mut m = Multiband::new(Arc::new(s));
            let mut impulse = vec![0.0; 2_000];
            impulse[0] = 1.0;
            m.process(&mut impulse);
            let peak = (0..impulse.len()).max_by(|&a, &b| impulse[a].abs().total_cmp(&impulse[b].abs()));
            (peak.unwrap(), m.latency_samples())
        };
        let (none, none_reported) = peak_at(0.0);
        let (two, two_reported) = peak_at(2.0);
        assert_eq!((none_reported, two_reported), (0, 96));
        assert_eq!(two - none, 96);
        let simple = Multiband::new(Arc::new(section()));
        assert_eq!(simple.latency_samples(), 0);
    }

    #[test]
    fn old_names_load_into_the_new_controls() {
        let settings = Settings::new(vec![]);
        settings.apply_text(
            "rack.format=2\n[stage 1]\nkind=Compressor\nadvanced=true\nAmount=25\nLow crossover=250\n\
             Attack=5\nRelease=200\nLow threshold=-30\nLow ratio=3.4\nLow makeup=2\nMid makeup=4\nHigh makeup=1\n",
        );
        let s = &settings.items()[0].section;
        let get = |strip, f| s.get(param(strip, f));
        assert_eq!((s.simple[0].get(), s.get(LOW_XOVER)), (25.0, 250.0));
        for band in 0..MASTER {
            assert_eq!((get(band, field::ATTACK), get(band, field::RELEASE)), (5.0, 200.0));
        }
        assert_eq!((get(0, field::THRESHOLD), get(0, field::RATIO)), (-30.0, 3.4));
        assert_eq!([get(0, field::OUTPUT), get(1, field::OUTPUT), get(2, field::OUTPUT)], [2.0, 4.0, 1.0]);
        assert_eq!((get(MASTER, field::ATTACK), get(MASTER, field::RATIO)), (10.0, 1.0), "master untouched");
    }

    #[test]
    fn defaults_leave_the_master_neutral_and_the_floor_off() {
        let s = section();
        let strips: Vec<Strip> = (0..STRIPS.len()).map(|k| Strip::read(k, &|i: usize| s.params[i].default)).collect();
        assert!(strips[MASTER].curve.is_flat());
        assert!(strips.iter().all(|s| s.curve.floor_ratio == 1.0 && s.curve.knee == 0.0 && s.input == 1.0));
        assert!(strips.iter().all(|s| !s.rms && s.hold == 0.0 && !s.solo && !s.mute && !s.bypass));
    }

    #[test]
    fn meters_show_which_band_is_working() {
        let s = Arc::new(advanced());
        let mut m = Multiband::new(s.clone());
        m.process(&mut sine(1_000.0, 48_000, 0.5));
        let (low, mid, high) = (s.take_reduction(0), s.take_reduction(1), s.take_reduction(2));
        assert!(mid > 3.0, "mid {mid}");
        assert!(low < 1.0 && high < 1.0, "low {low} high {high}");
        let (mid_in, mid_out) = (s.take_level(2), s.take_level(3));
        assert!(mid_in > 0.4 && mid_out < mid_in, "mid in {mid_in} out {mid_out}");
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
    fn output_gain_raises_the_band() {
        let s = advanced();
        set(&s, 1, field::OUTPUT, 6.0);
        let r = run(1_000.0, 0.01, s);
        assert!(r > 1.8, "{r}");
    }

    #[test]
    fn cost_with_every_strip_working_is_small() {
        let s = advanced();
        for strip in 0..STRIPS.len() {
            set(&s, strip, field::RATIO, 4.0);
            set(&s, strip, field::KNEE, 6.0);
            set(&s, strip, field::RMS, (strip % 2) as f32);
            set(&s, strip, field::FLOOR_RATIO, 2.0);
        }
        s.params[LOOKAHEAD].set(3.0);
        let mut m = Multiband::new(Arc::new(s));
        let seconds = 5.0;
        let mut audio = sine(300.0, (48_000.0 * seconds) as usize, 0.3);
        let started = std::time::Instant::now();
        for block in audio.chunks_exact_mut(480) {
            m.process(block);
        }
        let factor = started.elapsed().as_secs_f32() / seconds;
        eprintln!("compressor real-time factor: {factor:.4} (1.0 = one full core)");
        assert!(factor < 0.05, "compressor too heavy: {factor}");
    }
}
