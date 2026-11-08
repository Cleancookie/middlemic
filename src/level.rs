//! A level meter's motion: jumps up at once, holds a moment, then falls at a steady pace.
//! A peak tick rides above the bar and falls later, and a clip flag latches until cleared.

/// How long the bar stays at a peak before it starts to fall.
pub const HOLD_SECONDS: f32 = 0.4;
/// Seconds a full bar takes to fall to empty, unless the user picks another.
pub const DEFAULT_FALL_SECONDS: f32 = 1.5;
/// How long the peak tick stays at its high before it starts to fall.
pub const TICK_HOLD_SECONDS: f32 = 1.2;
/// Readings at or above this fraction (about -0.3 dBFS) count as clipping.
pub const CLIP_LEVEL: f32 = 0.995;
pub const FALL_RANGE: std::ops::RangeInclusive<f32> = 0.2..=4.0;

#[derive(Default)]
pub struct LevelMeter {
    level: f32,
    hold_left: f32,
    tick: f32,
    tick_hold_left: f32,
    clipped: bool,
}

impl LevelMeter {
    pub fn level(&self) -> f32 {
        self.level
    }

    /// The peak-hold tick: sits above the bar, falls after `TICK_HOLD_SECONDS`.
    pub fn tick(&self) -> f32 {
        self.tick
    }

    /// True from the first clipping reading until `clear_clip`.
    pub fn clipped(&self) -> bool {
        self.clipped
    }

    pub fn clear_clip(&mut self) {
        self.clipped = false;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `peak` is 0..1 and the newest reading; `dt` the seconds since the last call.
    pub fn update(&mut self, peak: f32, dt: f32, fall_seconds: f32) {
        self.clipped |= peak >= CLIP_LEVEL;
        if peak >= self.tick {
            self.tick = peak;
            self.tick_hold_left = TICK_HOLD_SECONDS;
        } else if self.tick_hold_left > 0.0 {
            self.tick_hold_left -= dt;
        } else {
            self.tick = (self.tick - dt / fall_seconds.max(0.01)).max(peak);
        }
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

    #[test]
    fn the_tick_outlives_the_bar_then_falls() {
        let mut m = LevelMeter::default();
        m.update(0.9, 0.05, 1.0);
        for _ in 0..16 {
            m.update(0.0, 0.05, 1.0);
        }
        assert!(m.level() < 0.9, "bar already falling");
        assert_eq!(m.tick(), 0.9, "tick held 0.8 s");
        for _ in 0..40 {
            m.update(0.0, 0.05, 1.0);
        }
        assert_eq!(m.tick(), 0.0);
        assert!(m.tick() >= m.level());
    }

    #[test]
    fn clipping_latches_until_cleared() {
        let mut m = LevelMeter::default();
        m.update(0.99, 0.05, 1.0);
        assert!(!m.clipped());
        m.update(1.0, 0.05, 1.0);
        for _ in 0..100 {
            m.update(0.0, 0.05, 1.0);
        }
        assert!(m.clipped());
        m.clear_clip();
        assert!(!m.clipped());
        m.update(1.0, 0.05, 1.0);
        m.reset();
        assert!(!m.clipped() && m.tick() == 0.0);
    }
}
