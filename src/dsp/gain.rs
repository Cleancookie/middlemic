use super::Processor;
use crate::settings::{Param, Section};
use std::sync::Arc;

pub const NAME: &str = "Mic boost";

pub fn section() -> Section {
    Section::new(
        NAME,
        vec![Param::new("Boost", "dB", 0.0, 30.0, 0.0).help(
            "How much louder to make your mic, in dB (decibels: +6 dB is about twice the signal, +10 dB sounds about twice as loud). \
             Start at 0 and raise it only if friends say you are quiet. Noise rises too, so set your Windows mic level first.",
        )],
    )
    .help("Turns your mic up before the stages below it. Smoothed, so dragging it does not click. Use it if your voice is too quiet.")
    .unfolded()
}

/// Smoothed gain, so dragging the slider does not click.
pub struct Gain {
    section: Arc<Section>,
    current: f32,
}

impl Gain {
    pub fn new(section: Arc<Section>) -> Self {
        let current = Self::target(&section);
        Self { section, current }
    }

    fn target(section: &Section) -> f32 {
        10f32.powf(section.get(0) / 20.0)
    }
}

impl Processor for Gain {
    fn enabled(&self) -> bool {
        self.section.is_enabled()
    }

    fn process(&mut self, samples: &mut [f32]) {
        let target = Self::target(&self.section);
        for s in samples {
            self.current += (target - self.current) * 0.001;
            *s *= self.current;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_db_roughly_doubles() {
        let s = Arc::new(section());
        s.params[0].set(6.0206);
        let mut g = Gain::new(s);
        let mut buf = [0.25_f32];
        g.process(&mut buf);
        assert!((buf[0] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn slider_move_ramps_instead_of_jumping() {
        let s = Arc::new(section());
        let mut g = Gain::new(s.clone());
        s.params[0].set(20.0);
        let mut buf = [1.0_f32; 4_800];
        g.process(&mut buf);
        assert!(buf[0] < 1.05, "first sample jumped to {}", buf[0]);
        assert!(buf[4_799] > 9.0, "never reached target: {}", buf[4_799]);
    }
}
