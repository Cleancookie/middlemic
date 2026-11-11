//! Running a recording through the rack all at once, as fast as the stages allow, and
//! saving the result.

use crate::dsp::{self, Chain};
use crate::settings::Settings;
use crate::wav;
use std::path::Path;

const FRAME: usize = (wav::RATE / 100) as usize;

/// Runs `samples` through `chain` in 10 ms frames, like the live engine. The chain's own delay is
/// cut from the front and the tail is flushed with silence, so the result lines up with the input.
/// `progress` is called with 0..1 now and then.
pub fn through_chain(chain: &mut Chain, samples: &[f32], mut progress: impl FnMut(f32)) -> Vec<f32> {
    let delay = chain.latency_samples();
    let total = samples.len() + delay;
    let mut out = Vec::with_capacity(total + FRAME);
    let mut frame = [0f32; FRAME];
    for (n, start) in (0..total).step_by(FRAME).enumerate() {
        for (i, slot) in frame.iter_mut().enumerate() {
            *slot = samples.get(start + i).copied().unwrap_or(0.0);
        }
        chain.process(&mut frame);
        out.extend_from_slice(&frame);
        if n % 50 == 0 {
            progress(start as f32 / total as f32);
        }
    }
    out.drain(..delay.min(out.len()));
    out.truncate(samples.len());
    out
}

/// Reads `source`, runs it through the rack as it is now, and writes `target`.
pub fn render_file(settings: &Settings, source: &Path, target: &Path, progress: impl FnMut(f32)) -> Result<(), String> {
    let (samples, rate) = wav::read_mono(source)?;
    if rate != wav::RATE {
        return Err(format!("{} Hz audio is not supported, only {} Hz", rate, wav::RATE));
    }
    let mut chain = dsp::build_chain(settings, wav::RATE).map_err(|e| format!("{e:#}"))?;
    let out = through_chain(&mut chain, &samples, progress);
    wav::write_mono(target, &out).map_err(|e| format!("could not save {}: {e}", target.display()))
}

/// `Take 3.wav` becomes `Take 3 (processed).wav`, next to it.
pub fn processed_name(source: &Path) -> std::path::PathBuf {
    let stem = source.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    source.with_file_name(format!("{stem} (processed).wav"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Processor;

    /// Doubles the signal and reports a delay it does not really have, as a lookahead stage would.
    struct Doubler(usize);

    impl Processor for Doubler {
        fn process(&mut self, samples: &mut [f32]) {
            samples.iter_mut().for_each(|s| *s *= 2.0);
        }

        fn latency_samples(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn output_has_the_input_length_and_goes_through_every_stage() {
        let mut chain = Chain::new();
        chain.push(1, Doubler(0));
        let input: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.05).sin() * 0.25).collect();
        let out = through_chain(&mut chain, &input, |_| {});
        assert_eq!(out.len(), input.len());
        assert!(out.iter().zip(&input).all(|(o, i)| (o - i * 2.0).abs() < 1e-6));
    }

    #[test]
    fn a_stage_that_delays_is_lined_up_again() {
        /// Really delays by `n` samples.
        struct Delay(Vec<f32>);
        impl Processor for Delay {
            fn process(&mut self, samples: &mut [f32]) {
                for s in samples {
                    self.0.push(*s);
                    *s = self.0.remove(0);
                }
            }
            fn latency_samples(&self) -> usize {
                self.0.len()
            }
        }
        let mut chain = Chain::new();
        chain.push(1, Delay(vec![0.0; 700]));
        let input: Vec<f32> = (0..3000).map(|i| (i % 97) as f32 / 97.0).collect();
        let out = through_chain(&mut chain, &input, |_| {});
        assert_eq!(out, input, "700 samples of delay removed, tail flushed");
    }

    #[test]
    fn progress_ends_before_one_and_runs_forwards() {
        let mut chain = Chain::new();
        let mut seen = Vec::new();
        through_chain(&mut chain, &vec![0.0; FRAME * 200], |p| seen.push(p));
        assert!(seen.len() >= 3 && seen.windows(2).all(|w| w[0] < w[1]) && seen.iter().all(|p| (0.0..1.0).contains(p)));
    }

    #[test]
    fn the_processed_copy_sits_next_to_the_take() {
        assert_eq!(processed_name(Path::new("/r/Take 3.wav")), Path::new("/r/Take 3 (processed).wav"));
    }

    #[test]
    fn a_file_is_rendered_through_the_rack_as_it_stands() {
        let dir = std::env::temp_dir().join(format!("middlemic-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (source, target) = (dir.join("in.wav"), dir.join("out.wav"));
        wav::write_mono(&source, &vec![0.1; 4800]).unwrap();
        let settings = Settings::new(vec![dsp::new_section("Mic boost").unwrap()]);
        render_file(&settings, &source, &target, |_| {}).unwrap();
        let (out, rate) = wav::read_mono(&target).unwrap();
        assert_eq!((out.len(), rate), (4800, wav::RATE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
