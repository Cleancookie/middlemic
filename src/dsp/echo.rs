use super::Processor;
use crate::settings::Section;
use ringbuf::{
    HeapCons,
    traits::{Consumer, Observer},
};
use sonora::config::EchoCanceller;
use sonora::{AudioProcessing, Config, StreamConfig};
use std::sync::Arc;

pub const NAME: &str = "Echo cancellation";

const SAMPLE_RATE: u32 = 48_000;
/// 10 ms, the only frame size the canceller takes.
const FRAME: usize = SAMPLE_RATE as usize / 100;
/// Reference audio piling up beyond this means the two devices' clocks drifted: drop the
/// oldest so the reference does not fall behind the microphone.
const MAX_BACKLOG_FRAMES: usize = 4;

/// No dials: the canceller tunes itself and finds the speaker-to-mic delay on its own.
/// Off by default because it needs the playback loopback to be running.
pub fn section() -> Section {
    Section::new(NAME, Vec::new()).off_by_default()
}

/// Removes what the speakers play from the microphone signal (WebRTC AEC3), so people on the
/// call stop hearing themselves. `reference` carries the audio going to the speakers.
/// Without one the stage does nothing. Put it first: it assumes the mic signal is still the
/// room plus the echo, before any non-linear stage has touched it.
pub struct EchoCancel {
    section: Arc<Section>,
    apm: AudioProcessing,
    reference: Option<HeapCons<f32>>,
    far: [f32; FRAME],
    out: [f32; FRAME],
}

impl EchoCancel {
    pub fn new(section: Arc<Section>, reference: Option<HeapCons<f32>>) -> Self {
        let stream = StreamConfig::new(SAMPLE_RATE, 1);
        let config = Config {
            echo_canceller: Some(EchoCanceller::default()),
            ..Default::default()
        };
        let apm = AudioProcessing::builder()
            .config(config)
            .capture_config(stream)
            .render_config(stream)
            .build();
        Self {
            section,
            apm,
            reference,
            far: [0.0; FRAME],
            out: [0.0; FRAME],
        }
    }

    /// The next 10 ms of speaker audio, or silence when none has arrived (the loopback
    /// delivers nothing while the speakers are quiet).
    fn next_reference(&mut self) {
        self.far.fill(0.0);
        let Some(ring) = self.reference.as_mut() else {
            return;
        };
        let backlog = ring.occupied_len();
        if backlog > MAX_BACKLOG_FRAMES * FRAME {
            ring.skip(backlog - 2 * FRAME);
        }
        if ring.occupied_len() >= FRAME {
            ring.pop_slice(&mut self.far);
        }
    }
}

impl Processor for EchoCancel {
    fn enabled(&self) -> bool {
        self.section.is_enabled() && self.reference.is_some()
    }

    fn prepare(&mut self, sample_rate: u32) {
        assert_eq!(sample_rate, SAMPLE_RATE, "echo cancellation needs 48 kHz");
    }

    fn process(&mut self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len() % FRAME, 0);
        for chunk in samples.chunks_exact_mut(FRAME) {
            self.next_reference();
            let rendered = self.apm.process_render_f32(&[&self.far], &mut [&mut self.out]);
            if rendered.is_err() {
                continue;
            }
            if self.apm.process_capture_f32(&[&*chunk], &mut [&mut self.out]).is_ok() {
                chunk.copy_from_slice(&self.out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::{
        HeapRb,
        traits::{Producer, Split},
    };

    /// Deterministic white noise with a slow on/off envelope, a stand-in for speech.
    fn far_end(n: usize) -> Vec<f32> {
        let mut state = 0x9e37_79b9_u32;
        (0..n)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let envelope = 0.6 + 0.4 * ((i as f32 / SAMPLE_RATE as f32) * 3.0).sin();
                (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.3 * envelope
            })
            .collect()
    }

    /// The far end as the microphone hears it: 60 ms later, with two weaker reflections.
    fn echo_of(far: &[f32]) -> Vec<f32> {
        let tap = |i: usize, delay: usize| if i >= delay { far[i - delay] } else { 0.0 };
        (0..far.len())
            .map(|i| 0.35 * tap(i, 2880) + 0.15 * tap(i, 3120) + 0.08 * tap(i, 3456))
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Run the mic through the stage, handing it the matching far-end frame each time.
    fn run(mic: &[f32], far: &[f32]) -> Vec<f32> {
        let (mut prod, cons) = HeapRb::<f32>::new(far.len()).split();
        let mut stage = EchoCancel::new(Arc::new(section()), Some(cons));
        let mut out = mic.to_vec();
        for (m, f) in out.chunks_exact_mut(FRAME).zip(far.chunks_exact(FRAME)) {
            prod.push_slice(f);
            stage.process(m);
        }
        out
    }

    #[test]
    fn removes_the_speakers_from_the_mic() {
        let far = far_end(SAMPLE_RATE as usize * 8);
        let mic = echo_of(&far);
        let out = run(&mic, &far);
        let tail = SAMPLE_RATE as usize * 5..;
        let removed_db = 20.0 * (rms(&mic[tail.clone()]) / rms(&out[tail])).log10();
        eprintln!("echo removed: {removed_db:.1} dB");
        assert!(removed_db > 15.0, "echo only reduced by {removed_db:.1} dB");
    }

    #[test]
    fn keeps_a_voice_talking_over_the_echo() {
        let far = far_end(SAMPLE_RATE as usize * 8);
        let voice: Vec<f32> = (0..far.len())
            .map(|i| 0.15 * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / SAMPLE_RATE as f32).sin())
            .collect();
        let echo = echo_of(&far);
        let talking_from = SAMPLE_RATE as usize * 5;
        let mic: Vec<f32> = (0..far.len())
            .map(|i| echo[i] + if i >= talking_from { voice[i] } else { 0.0 })
            .collect();
        let out = run(&mic, &far);
        let tail = SAMPLE_RATE as usize * 6..;
        let kept = rms(&out[tail.clone()]) / rms(&voice[tail]);
        eprintln!("voice kept: {:.0}%", kept * 100.0);
        assert!(kept > 0.5, "only {:.0}% of the voice survived", kept * 100.0);
    }

    #[test]
    fn does_nothing_without_a_reference() {
        let stage = EchoCancel::new(Arc::new(Section::new(NAME, Vec::new())), None);
        assert!(!stage.enabled());
    }

    #[test]
    fn starts_switched_off_and_resets_to_off() {
        let s = section();
        assert!(!s.is_enabled());
        s.enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        s.reset();
        assert!(!s.is_enabled());
    }
}
