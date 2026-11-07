//! Feedback (howl) suppression. An FFT of the stage's own output looks for narrow, steady
//! peaks that stand far above their neighbourhood, and gives each a narrow notch that deepens
//! while the howl persists and lets go once it is gone. Only the notches are in the audio
//! path; the analysis just steers them, so no delay is added.

use super::Processor;
use super::biquad::{Biquad, Coeffs};
use crate::settings::{Param, Section};
use crate::spectrum::{Analyzer, FFT_LEN};
use std::sync::Arc;

pub const NAME: &str = "Feedback suppressor";

const SENSITIVITY: usize = 0;
const MAX_NOTCHES: usize = 1;
pub const MAX_DEPTH: usize = 2;
const RELEASE: usize = 3;

/// The most notches a stage holds, whatever Max notches says.
pub const SLOTS: usize = 12;
const Q: f32 = 40.0;
const MIN_HZ: f32 = 150.0;
const MAX_HZ: f32 = 16_000.0;
/// Quieter peaks are left alone (dBFS, a sine's level).
const LEVEL_GATE_DB: f32 = -50.0;
/// A peak with a partner an octave up no more than this much quieter is a voice harmonic.
const HARMONIC_DB: f32 = 30.0;
/// A peak is compared with the mean of the bins this many octaves either side, less its own.
const NEIGHBOURHOOD_OCTAVES: f32 = 1.0 / 3.0;
const GUARD_BINS: usize = 3;
/// A howl is a lone line: every other bin in its neighbourhood is at least this much quieter.
/// Voice harmonics come in combs, so a neighbour is close in level.
const LONE_DB: f32 = 12.0;
/// A howl holds its pitch; a track that strays this many bins from its start is not one.
const DRIFT_BINS: f32 = 0.6;
/// Frames a track may skip, and how far it may fall from its loudest, before it is dropped.
const MISSES: u32 = 2;
const DECAY_DB: f32 = 6.0;
const STEP_DB: f32 = 3.0;
/// Frames a confirmed howl must last again before its notch deepens another step.
const REPEAT_FRAMES: u32 = 5;
/// A notch not heard for this many frames starts its release.
const HOLD_FRAMES: u64 = 50;
const SLEW_DB_PER_S: f32 = 60.0;
/// Coefficients are refreshed this often (samples) while a notch's depth moves.
const SUB_BLOCK: usize = 16;
const MAX_TRACKS: usize = 16;
const MAX_CANDIDATES: usize = 32;

pub fn section() -> Section {
    Section::new(
        NAME,
        vec![
            Param::new("Sensitivity", "%", 0.0, 100.0, 50.0).help(
                "How readily a steady tone counts as feedback. Higher catches a howl sooner but may cut a long \
                 sung note. Start at 50. Raise it if howls still get through.",
            ),
            Param::new("Max notches", "notches", 1.0, SLOTS as f32, 8.0).help(
                "How many howling notes it can cut at once. Each cut is very narrow. \
                 Start at 8. Lower it if your voice ever sounds hollow.",
            ),
            Param::new("Max depth", "dB", 3.0, 30.0, 18.0).help(
                "How deep each cut can go, in dB. Deeper stops louder howls. \
                 Start at 18. Raise it if a howl still rings through.",
            ),
            Param::new("Release", "s", 1.0, 60.0, 10.0).log().help(
                "How long, in seconds, a cut takes to fade once the howl stops. Longer keeps you safe while the \
                 speakers stay loud. Start at 10. Shorten it if you often move the mic or speakers.",
            ),
        ],
    )
    .help(
        "Stops the howl or squeal when your speakers feed back into your mic. Echo is sound repeating back; \
         feedback is one note that keeps growing. It cuts each howling note with a very narrow notch, so your \
         voice stays untouched. Works best first in the chain.",
    )
    .with_reduction_meters(&["Feedback"])
    .with_readouts(2 * SLOTS)
}

/// The notches in use, as (frequency Hz, depth dB), for the display.
pub fn notches(section: &Section) -> impl Iterator<Item = (f32, f32)> + '_ {
    (0..SLOTS)
        .map(|i| (section.readout(2 * i), section.readout(2 * i + 1)))
        .filter(|&(freq, depth)| freq > 0.0 && depth > 0.05)
}

/// Lets every notch go. The display empties at once, also while the audio is stopped.
pub fn clear(section: &Section) {
    section.request_clear();
    (0..2 * SLOTS).for_each(|slot| section.set_readout(slot, 0.0));
}

/// Peak-to-neighbourhood ratio (dB) and frames of persistence that confirm a howl.
fn detection(sensitivity: f32) -> (f32, u32) {
    let s = sensitivity.clamp(0.0, 100.0) / 100.0;
    (32.0 - 20.0 * s, (16.0 - 8.0 * s).round() as u32)
}

#[derive(Default)]
struct Notch {
    filter: Biquad,
    /// 0 when the slot is free.
    freq: f32,
    /// Cut in dB, positive.
    depth: f32,
    target: f32,
    born: u64,
    seen: u64,
    /// The frame the release began, and the target then.
    release: Option<(u64, f32)>,
}

impl Notch {
    fn is_free(&self) -> bool {
        self.freq == 0.0
    }

    fn near(&self, freq: f32, bin_hz: f32) -> bool {
        !self.is_free() && (self.freq - freq).abs() <= (self.freq / (2.0 * Q)).max(bin_hz)
    }
}

/// A peak followed across frames.
struct Track {
    start: f32,
    freq: f32,
    frames: u32,
    misses: u32,
    loudest: f32,
    last: f32,
    matched: bool,
    confirmed: bool,
}

struct Candidate {
    freq: f32,
    db: f32,
}

pub struct FeedbackSuppressor {
    section: Arc<Section>,
    fs: f32,
    /// Samples per analysis frame: 10 ms.
    hop: usize,
    since: usize,
    frame: u64,
    /// The latest output, oldest first.
    history: Vec<f32>,
    analyzer: Analyzer,
    power: Vec<f32>,
    prefix: Vec<f64>,
    candidates: Vec<Candidate>,
    tracks: Vec<Track>,
    notches: [Notch; SLOTS],
}

impl FeedbackSuppressor {
    pub fn new(section: Arc<Section>) -> Self {
        let bins = FFT_LEN / 2 + 1;
        let mut s = Self {
            section,
            fs: 48_000.0,
            hop: 480,
            since: 0,
            frame: 0,
            history: vec![0.0; FFT_LEN],
            analyzer: Analyzer::default(),
            power: vec![0.0; bins],
            prefix: vec![0.0; bins + 1],
            candidates: Vec::with_capacity(MAX_CANDIDATES),
            tracks: Vec::with_capacity(MAX_TRACKS),
            notches: Default::default(),
        };
        s.prepare(48_000);
        s
    }

    fn bin_hz(&self) -> f32 {
        self.fs / FFT_LEN as f32
    }

    fn limit(&self) -> usize {
        (self.section.get(MAX_NOTCHES).round() as usize).clamp(1, SLOTS)
    }

    /// Runs the notches, moving each one's depth towards its target a little per sub-block.
    fn filter(&mut self, chunk: &mut [f32]) {
        for part in chunk.chunks_mut(SUB_BLOCK) {
            let step = SLEW_DB_PER_S * part.len() as f32 / self.fs;
            for n in self.notches.iter_mut().filter(|n| !n.is_free()) {
                if n.depth != n.target {
                    if n.depth == 0.0 {
                        n.filter = Biquad::default();
                    }
                    n.depth = n.target.clamp(n.depth - step, n.depth + step);
                    n.filter.set(Coeffs::peaking(self.fs, n.freq, Q, -n.depth));
                }
                if n.depth > 0.0 {
                    part.iter_mut().for_each(|s| *s = n.filter.tick(*s));
                }
            }
        }
    }

    fn remember(&mut self, chunk: &[f32]) {
        let n = chunk.len();
        self.history.copy_within(n.., 0);
        self.history[FFT_LEN - n..].copy_from_slice(chunk);
    }

    /// Collects this frame's howl candidates: narrow, loud peaks with no octave partner.
    fn find_candidates(&mut self, threshold_db: f32) {
        for (p, v) in self.power.iter_mut().zip(self.analyzer.power(&self.history)) {
            *p = v;
        }
        for k in 0..self.power.len() {
            self.prefix[k + 1] = self.prefix[k] + self.power[k] as f64;
        }
        self.candidates.clear();
        let power = &self.power;
        let bin_hz = self.fs / FFT_LEN as f32;
        let last = power.len() - 1;
        let first = ((MIN_HZ / bin_hz).ceil() as usize).max(2 * GUARD_BINS + 1);
        let end = ((MAX_HZ.min(self.fs * 0.45) / bin_hz) as usize).min(last - 2);
        let gate = 10f32.powf(LEVEL_GATE_DB / 10.0);
        let ratio = 10f64.powf(threshold_db as f64 / 10.0);
        let partner = 10f32.powf(-HARMONIC_DB / 10.0);
        let lone = 10f32.powf(-LONE_DB / 10.0);
        let span = 2f32.powf(NEIGHBOURHOOD_OCTAVES);
        let db = |i: usize| 10.0 * power[i].max(1e-20).log10();
        for k in first..=end {
            let p = power[k];
            if p < gate || p <= power[k - 1] || p < power[k + 1] || p <= power[k - 2] || p < power[k + 2] {
                continue;
            }
            let lo = ((k as f32 / span) as usize).min(k - 2 * GUARD_BINS);
            let hi = ((k as f32 * span).ceil() as usize).max(k + 2 * GUARD_BINS).min(last);
            let own = self.prefix[k + GUARD_BINS + 1] - self.prefix[k - GUARD_BINS];
            let around = (self.prefix[hi + 1] - self.prefix[lo] - own) / (hi - lo - 2 * GUARD_BINS) as f64;
            if (p as f64) < ratio * around.max(1e-30) {
                continue;
            }
            let mut neighbours = power[lo..k - GUARD_BINS].iter().chain(&power[k + GUARD_BINS + 1..=hi]);
            if neighbours.any(|&q| q > p * lone) {
                continue;
            }
            let (a, b, c) = (db(k - 1), db(k), db(k + 1));
            let bend = a - 2.0 * b + c;
            let offset = if bend < 0.0 { (0.5 * (a - c) / bend).clamp(-0.5, 0.5) } else { 0.0 };
            // A partner that is itself a notched howl does not count.
            let freq = (k as f32 + offset) * bin_hz;
            let octave = (2.0 * (k as f32 + offset)).round() as usize;
            if octave < last
                && power[octave - 1..=octave + 1].iter().any(|&q| q > p * partner)
                && !self.notches.iter().any(|n| n.near(2.0 * freq, bin_hz))
            {
                continue;
            }
            self.candidates.push(Candidate { freq, db: b - 0.25 * (a - c) * offset });
            if self.candidates.len() == MAX_CANDIDATES {
                break;
            }
        }
    }

    /// Follows the candidates across frames, confirming those that hold steady for `persist` frames.
    fn follow_tracks(&mut self, persist: u32) {
        let drift = DRIFT_BINS * self.bin_hz();
        self.tracks.iter_mut().for_each(|t| t.matched = false);
        for c in &self.candidates {
            let distance = |t: &Track| (t.start - c.freq).abs();
            let nearest = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.matched && distance(t) <= drift)
                .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
                .map(|(i, _)| i);
            match nearest {
                Some(i) => {
                    let t = &mut self.tracks[i];
                    t.matched = true;
                    t.frames += 1;
                    t.misses = 0;
                    t.freq = c.freq;
                    t.last = c.db;
                    t.loudest = t.loudest.max(c.db);
                }
                None if self.tracks.len() < MAX_TRACKS => self.tracks.push(Track {
                    start: c.freq,
                    freq: c.freq,
                    frames: 1,
                    misses: 0,
                    loudest: c.db,
                    last: c.db,
                    matched: true,
                    confirmed: false,
                }),
                None => {}
            }
        }
        self.tracks.retain_mut(|t| {
            t.misses += u32::from(!t.matched);
            t.misses <= MISSES && t.last >= t.loudest - DECAY_DB
        });
        for i in 0..self.tracks.len() {
            let t = &mut self.tracks[i];
            let needed = if t.confirmed { REPEAT_FRAMES } else { persist };
            if t.matched && t.frames >= needed {
                (t.frames, t.confirmed, t.loudest) = (0, true, t.last);
                let freq = t.freq;
                self.confirm(freq);
            }
        }
    }

    /// Deepens the notch at `freq`, or puts a new one there: in a free slot, else in place of
    /// the oldest one letting go, else the shallowest.
    fn confirm(&mut self, freq: f32) {
        let (bin_hz, frame) = (self.bin_hz(), self.frame);
        let max_depth = self.section.get(MAX_DEPTH);
        let limit = self.limit();
        let slots = &mut self.notches[..limit];
        let index = slots
            .iter()
            .position(|n| n.near(freq, bin_hz))
            .or_else(|| slots.iter().position(Notch::is_free))
            .or_else(|| {
                let releasing = slots.iter().enumerate().filter(|(_, n)| n.release.is_some());
                releasing.min_by_key(|(_, n)| n.born).map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                let shallowest = slots.iter().enumerate().min_by(|a, b| a.1.depth.total_cmp(&b.1.depth));
                shallowest.map_or(0, |(i, _)| i)
            });
        let n = &mut slots[index];
        if !n.near(freq, bin_hz) {
            *n = Notch { freq, born: frame, ..Notch::default() };
        }
        n.target = (n.target + STEP_DB).min(max_depth);
        n.seen = frame;
        n.release = None;
    }

    /// Once a frame: keeps notches that are still heard, and lets the others go.
    fn steer(&mut self) {
        let bin_hz = self.bin_hz();
        let frame_s = self.hop as f32 / self.fs;
        let release_s = self.section.get(RELEASE);
        let max_depth = self.section.get(MAX_DEPTH);
        let limit = self.limit();
        for (i, n) in self.notches.iter_mut().enumerate().filter(|(_, n)| !n.is_free()) {
            if self.candidates.iter().any(|c| n.near(c.freq, bin_hz)) {
                n.seen = self.frame;
                n.release = None;
            }
            if n.release.is_none() && self.frame - n.seen > HOLD_FRAMES {
                n.release = Some((self.frame, n.target));
            }
            if let Some((start, from)) = n.release {
                let gone = (self.frame - start) as f32 * frame_s / release_s;
                n.target = from * (1.0 - gone).max(0.0);
            }
            n.target = if i < limit { n.target.min(max_depth) } else { 0.0 };
            if n.target == 0.0 && n.depth == 0.0 {
                *n = Notch::default();
            }
        }
    }

    fn analyse(&mut self) {
        let (threshold, persist) = detection(self.section.get(SENSITIVITY));
        self.find_candidates(threshold);
        self.follow_tracks(persist);
        self.steer();
    }

    /// Lets every notch fade out quickly and forgets every track.
    fn clear(&mut self) {
        self.tracks.clear();
        for n in &mut self.notches {
            n.target = 0.0;
            n.release = Some((self.frame, 0.0));
        }
    }

    fn publish(&self) {
        let mut deepest = 0f32;
        for (i, n) in self.notches.iter().enumerate() {
            self.section.set_readout(2 * i, n.freq);
            self.section.set_readout(2 * i + 1, n.depth);
            deepest = deepest.max(n.depth);
        }
        self.section.record_reduction(0, deepest);
    }
}

impl Processor for FeedbackSuppressor {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn prepare(&mut self, sample_rate: u32) {
        self.fs = sample_rate as f32;
        self.hop = (sample_rate as usize / 100).clamp(1, FFT_LEN);
        self.since = 0;
        self.history.fill(0.0);
        self.tracks.clear();
        self.notches = Default::default();
    }

    fn process(&mut self, samples: &mut [f32]) {
        if self.section.take_clear_request() {
            self.clear();
        }
        let mut rest = samples;
        while !rest.is_empty() {
            let (chunk, tail) = rest.split_at_mut((self.hop - self.since).min(rest.len()));
            self.filter(chunk);
            self.remember(chunk);
            self.since += chunk.len();
            if self.since == self.hop {
                self.since = 0;
                self.frame += 1;
                self.analyse();
            }
            rest = tail;
        }
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::{rms, sine};
    use std::f32::consts::PI;

    const FS: f32 = 48_000.0;
    const BLOCK: usize = 480;

    /// Pink-ish noise (Paul Kellet's economy filter on a fixed-seed white noise) at `level` RMS.
    fn noise(n: usize, level: f32, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let mut b = [0f32; 3];
        let raw: Vec<f32> = (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let white = state as f32 / u32::MAX as f32 * 2.0 - 1.0;
                b[0] = 0.997_65 * b[0] + white * 0.099_046;
                b[1] = 0.963 * b[1] + white * 0.296_516_4;
                b[2] = 0.57 * b[2] + white * 1.052_691_3;
                b[0] + b[1] + b[2] + white * 0.184_8
            })
            .collect();
        let gain = level / rms(&raw);
        raw.into_iter().map(|s| s * gain).collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    fn db(x: f32) -> f32 {
        20.0 * x.max(1e-9).log10()
    }

    /// Amplitude of the `freq` component of `x`.
    fn tone_level(x: &[f32], freq: f32) -> f32 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, &s) in x.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * freq as f64 * i as f64 / FS as f64;
            re += s as f64 * phase.cos();
            im += s as f64 * phase.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    fn fresh() -> (Arc<Section>, FeedbackSuppressor) {
        let s = Arc::new(section());
        (s.clone(), FeedbackSuppressor::new(s))
    }

    /// Runs `audio` through in 10 ms blocks, calling `each` with the block's start time.
    fn run(f: &mut FeedbackSuppressor, audio: &mut [f32], mut each: impl FnMut(f32)) {
        for (i, block) in audio.chunks_mut(BLOCK).enumerate() {
            f.process(block);
            each(i as f32 * BLOCK as f32 / FS);
        }
    }

    #[test]
    fn a_steady_tone_over_noise_is_notched_quickly_and_deeply() {
        for freq in [1_000.0, 1_733.0, 2_900.0] {
            let (s, mut f) = fresh();
            let tone = sine(freq, 96_000, 0.1);
            let input = mix(&tone, &noise(96_000, rms(&tone) / 10f32.powf(25.0 / 20.0), 1));
            let mut output = input.clone();
            let mut caught = None;
            run(&mut f, &mut output, |t| {
                if caught.is_none() && notches(&s).count() > 0 {
                    caught = Some(t);
                }
            });
            let caught = caught.expect("never notched");
            assert!(caught <= 0.2, "{freq} Hz notched after {caught} s");
            let found: Vec<_> = notches(&s).collect();
            assert_eq!(found.len(), 1, "{freq} Hz: {found:?}");
            assert!((found[0].0 - freq).abs() / freq < 0.005, "{freq} Hz notch at {}", found[0].0);
            let tail = 72_000..;
            let drop = db(tone_level(&input[tail.clone()], freq)) - db(tone_level(&output[tail], freq));
            assert!(drop >= 10.0, "{freq} Hz only {drop:.1} dB down");
        }
    }

    #[test]
    fn a_growing_howl_is_caught_while_still_quiet() {
        let (s, mut f) = fresh();
        let freq = 2_400.0;
        let n = 72_000;
        let amp = |i: usize| 1e-3 * 10f32.powf(2.0 * i as f32 / FS);
        let howl: Vec<f32> = (0..n).map(|i| amp(i) * (2.0 * PI * freq * i as f32 / FS).sin()).collect();
        let input = mix(&howl, &noise(n, 0.003, 2));
        let mut output = input.clone();
        let mut caught_at = None;
        run(&mut f, &mut output, |t| {
            if caught_at.is_none() && notches(&s).any(|(f, _)| (f - freq).abs() < 20.0) {
                caught_at = Some(amp((t * FS) as usize));
            }
        });
        let level = caught_at.expect("the howl was never caught");
        assert!(level < 0.03, "caught only at {:.1} dBFS", db(level));
        let tail = n - 9_600..;
        let drop = db(tone_level(&input[tail.clone()], freq)) - db(tone_level(&output[tail], freq));
        assert!(drop >= 10.0, "only {drop:.1} dB down at the end");
    }

    #[test]
    fn a_real_feedback_loop_is_stopped() {
        // Speaker to mic: a resonance at 2.4 kHz with a little more than unity gain, one block late.
        let howl_in = |f: &mut Option<FeedbackSuppressor>| {
            let mut room = Biquad::new(Coeffs::peaking(FS, 2_400.0, 30.0, 12.0));
            let mic_noise = noise(144_000, 0.003, 3);
            let mut back = vec![0f32; BLOCK];
            let mut out = Vec::new();
            for chunk in mic_noise.chunks(BLOCK) {
                let mut block: Vec<f32> = chunk.iter().zip(&back).map(|(n, b)| (n + b).clamp(-1.0, 1.0)).collect();
                if let Some(f) = f {
                    f.process(&mut block);
                }
                back = block.iter().map(|&s| 0.3 * room.tick(s)).collect();
                out.extend(block);
            }
            rms(&out[96_000..])
        };
        let open = howl_in(&mut None);
        let suppressed = howl_in(&mut Some(fresh().1));
        assert!(open > 0.3, "the loop should howl on its own: {open}");
        assert!(suppressed < 0.02, "still howling: {suppressed}");
    }

    /// Voiced speech: harmonics of a gliding f0 shaped by moving formants, in syllables.
    fn voice(seconds: f32) -> Vec<f32> {
        const FORMANTS: [(f32, f32); 4] = [(700.0, 1_200.0), (300.0, 2_300.0), (500.0, 1_800.0), (600.0, 1_000.0)];
        let resonance = |f: f32, centre: f32, width: f32| 1.0 / (1.0 + ((f - centre) / width).powi(2));
        let mut phase = 0f32;
        let raw: Vec<f32> = (0..(seconds * FS) as usize)
            .map(|i| {
                let t = i as f32 / FS;
                let f0 = 140.0 + 40.0 * (2.0 * PI * 0.7 * t).sin();
                phase = (phase + 2.0 * PI * f0 / FS) % (2.0 * PI);
                let syllable = t * 4.0;
                let within = syllable.fract();
                let envelope = if within < 0.8 { (PI * within / 0.8).sin() } else { 0.0 };
                let (f1, f2) = FORMANTS[syllable as usize % FORMANTS.len()];
                let voiced: f32 = (1..)
                    .map(|h| h as f32)
                    .take_while(|h| h * f0 < 5_000.0)
                    .map(|h| (resonance(h * f0, f1, 90.0) + resonance(h * f0, f2, 120.0)) / h * (h * phase).sin())
                    .sum();
                envelope * voiced
            })
            .collect();
        let peak = raw.iter().fold(0f32, |m, s| m.max(s.abs()));
        raw.iter().map(|s| s * 0.3 / peak).collect()
    }

    #[test]
    fn speech_is_never_notched() {
        let speech = mix(&voice(6.0), &noise(288_000, 0.001, 4));
        // The default, and some headroom above it.
        for sensitivity in [50.0, 75.0] {
            let (s, mut f) = fresh();
            s.params[SENSITIVITY].set(sensitivity);
            let mut worst = 0;
            run(&mut f, &mut speech.clone(), |_| worst = worst.max(notches(&s).count()));
            assert_eq!(worst, 0, "speech was notched at sensitivity {sensitivity}");
            assert_eq!(s.take_reduction(0), 0.0);
        }
    }

    #[test]
    fn notches_are_freed_after_the_howl_stops() {
        let (s, mut f) = fresh();
        s.params[RELEASE].set(1.0);
        let mut howl = mix(&sine(1_200.0, 48_000, 0.2), &noise(48_000, 0.002, 5));
        run(&mut f, &mut howl, |_| {});
        assert_eq!(notches(&s).count(), 1);
        let mut quiet = noise(48_000 * 3, 0.002, 6);
        let mut last_seen = 0.0;
        run(&mut f, &mut quiet, |t| {
            if notches(&s).count() > 0 {
                last_seen = t;
            }
        });
        assert!(last_seen > 1.0, "released too soon, at {last_seen} s");
        assert!(last_seen < 2.0, "still there at {last_seen} s");
    }

    #[test]
    fn clearing_lets_every_notch_go() {
        let (s, mut f) = fresh();
        let mut howl = mix(&sine(1_200.0, 48_000, 0.2), &noise(48_000, 0.002, 7));
        run(&mut f, &mut howl, |_| {});
        assert_eq!(notches(&s).count(), 1);
        clear(&s);
        assert_eq!(notches(&s).count(), 0, "the display empties at once");
        let mut quiet = noise(24_000, 0.002, 8);
        run(&mut f, &mut quiet, |_| {});
        assert_eq!(notches(&s).count(), 0);
        assert!(f.notches.iter().all(Notch::is_free));
    }

    #[test]
    fn a_deepening_notch_does_not_click() {
        let (s, mut f) = fresh();
        let input = sine(1_500.0, 48_000, 0.5);
        let jump = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        let mut output = input.clone();
        run(&mut f, &mut output, |_| {});
        assert!(notches(&s).next().is_some_and(|(_, d)| d > 10.0), "never deepened");
        assert!(jump(&output) <= jump(&input) * 1.02, "jump {} vs {}", jump(&output), jump(&input));
    }

    #[test]
    fn the_notch_limit_and_depth_are_respected() {
        let (s, mut f) = fresh();
        s.params[MAX_NOTCHES].set(2.0);
        s.params[MAX_DEPTH].set(6.0);
        let tones = [500.0, 1_100.0, 2_300.0, 4_700.0].map(|freq| sine(freq, 96_000, 0.05));
        let mut audio = tones.iter().fold(noise(96_000, 0.001, 9), |acc, t| mix(&acc, t));
        let mut most = 0;
        run(&mut f, &mut audio, |_| {
            let found: Vec<_> = notches(&s).collect();
            most = most.max(found.len());
            assert!(found.iter().all(|&(_, d)| d <= 6.0 + 1e-3), "{found:?}");
        });
        assert_eq!(most, 2);
    }

    #[test]
    fn howls_an_octave_apart_are_both_caught() {
        let (s, mut f) = fresh();
        let mut audio = mix(&mix(&sine(1_100.0, 96_000, 0.1), &sine(2_200.0, 96_000, 0.1)), &noise(96_000, 0.001, 11));
        run(&mut f, &mut audio, |_| {});
        assert_eq!(notches(&s).count(), 2, "{:?}", notches(&s).collect::<Vec<_>>());
    }

    #[test]
    fn cost_with_eight_notches_working_is_small() {
        let (s, mut f) = fresh();
        let seconds = 5.0;
        let n = (FS * seconds) as usize;
        let tones = [300.0, 435.0, 630.0, 915.0, 1_327.0, 1_924.0, 2_790.0, 4_045.0].map(|freq| sine(freq, n, 0.03));
        let mut audio = tones.iter().fold(noise(n, 0.001, 10), |acc, t| mix(&acc, t));
        let started = std::time::Instant::now();
        run(&mut f, &mut audio, |_| {});
        let factor = started.elapsed().as_secs_f32() / seconds;
        eprintln!("feedback suppressor real-time factor: {factor:.4} with {} notches (1.0 = one full core)", notches(&s).count());
        assert_eq!(notches(&s).count(), 8);
        assert!(factor < 0.01, "feedback suppressor too heavy: {factor}");
    }

    /// Counts notches put on a real recording of speech.
    /// `MIDDLEMIC_TEST_AUDIO=<file> cargo test --release feedback_on_recording -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn feedback_on_recording() {
        let path = std::env::var("MIDDLEMIC_TEST_AUDIO").expect("set MIDDLEMIC_TEST_AUDIO");
        let mut audio = crate::offline::to_48k(&crate::offline::decode(std::path::Path::new(&path)));
        let (s, mut f) = fresh();
        let mut hits = Vec::new();
        run(&mut f, &mut audio, |t| {
            for (freq, depth) in notches(&s) {
                if hits.last().is_none_or(|&(_, f, _)| f != freq) {
                    hits.push((t, freq, depth));
                }
            }
        });
        for (t, freq, depth) in &hits {
            println!("{t:7.2} s  {freq:7.1} Hz  {depth:4.1} dB");
        }
        assert!(hits.is_empty(), "{} notches on speech", hits.len());
    }
}
