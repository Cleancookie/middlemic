//! A level meter's motion: jumps up at once, holds a moment, then falls at a steady pace.

/// How long the bar stays at a peak before it starts to fall.
pub const HOLD_SECONDS: f32 = 0.4;
/// Seconds a full bar takes to fall to empty, unless the user picks another.
pub const DEFAULT_FALL_SECONDS: f32 = 1.5;
pub const FALL_RANGE: std::ops::RangeInclusive<f32> = 0.2..=4.0;

#[derive(Default)]
pub struct LevelMeter {
    level: f32,
    hold_left: f32,
}

impl LevelMeter {
    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `peak` is 0..1 and the newest reading; `dt` the seconds since the last call.
    pub fn update(&mut self, peak: f32, dt: f32, fall_seconds: f32) {
        if peak >= self.level {
            self.level = peak;
            self.hold_left = HOLD_SECONDS;
        } else if self.hold_left > 0.0 {
            self.hold_left -= dt;
        } else {
            self.level = (self.level - dt / fall_seconds.max(0.01)).max(peak);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_at_once() {
        let mut m = LevelMeter::default();
        m.update(0.8, 0.05, 1.5);
        assert_eq!(m.level(), 0.8);
    }

    #[test]
    fn holds_then_falls_at_the_chosen_pace() {
        let mut m = LevelMeter::default();
        m.update(1.0, 0.05, 1.0);
        for _ in 0..7 {
            m.update(0.0, 0.05, 1.0);
        }
        assert_eq!(m.level(), 1.0, "still held after 0.35 s");
        for _ in 0..4 {
            m.update(0.0, 0.05, 1.0);
        }
        assert!(m.level() < 1.0 && m.level() > 0.7, "falling slowly: {}", m.level());
        for _ in 0..20 {
            m.update(0.0, 0.05, 1.0);
        }
        assert_eq!(m.level(), 0.0);
    }

    #[test]
    fn a_louder_peak_restarts_the_hold() {
        let mut m = LevelMeter::default();
        m.update(0.5, 0.05, 1.0);
        for _ in 0..7 {
            m.update(0.0, 0.05, 1.0);
        }
        m.update(0.6, 0.05, 1.0);
        for _ in 0..7 {
            m.update(0.0, 0.05, 1.0);
        }
        assert_eq!(m.level(), 0.6);
    }
}
