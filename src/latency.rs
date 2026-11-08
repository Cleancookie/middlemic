//! End-to-end delay from the mic to the virtual cable, as the parts it is made of.

/// The DSP thread works on blocks this long, so a sample waits for its block to fill.
pub const BLOCK_MS: usize = 10;
/// The output holds back this much audio before it starts, so uneven delivery is absorbed.
pub const CUSHION_MS: usize = 50;

/// One entry per place the audio waits, in the order it meets them.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Latency {
    pub capture_ms: f32,
    pub block_ms: f32,
    pub stages_ms: f32,
    pub cushion_ms: f32,
    pub render_ms: f32,
}

impl Latency {
    /// `capture_ms` and `render_ms` are the device buffers Windows reported when they were
    /// opened; `stages_ms` is the sum of the enabled stages' own delays.
    pub fn new(capture_ms: f32, stages_ms: f32, render_ms: f32) -> Self {
        Self {
            capture_ms,
            block_ms: BLOCK_MS as f32,
            stages_ms,
            cushion_ms: CUSHION_MS as f32,
            render_ms,
        }
    }

    pub fn total_ms(&self) -> f32 {
        self.capture_ms + self.block_ms + self.stages_ms + self.cushion_ms + self.render_ms
    }

    /// Label and delay of each part, for the tooltip.
    pub fn parts(&self) -> [(&'static str, f32); 5] {
        [
            ("Mic buffer", self.capture_ms),
            ("Processing block", self.block_ms),
            ("Stage delays", self.stages_ms),
            ("Playout cushion", self.cushion_ms),
            ("Output buffer", self.render_ms),
        ]
    }

    pub fn breakdown(&self) -> String {
        let mut lines: Vec<String> = self.parts().iter().map(|(name, ms)| format!("{name}: {ms:.0} ms")).collect();
        lines.push("A worst-case estimate: Windows adds a little more inside its audio engine.".to_string());
        lines.join("\n")
    }
}

/// A stage's load (1.0 is one whole core) as a percentage of one core.
pub fn load_text(load: f32) -> String {
    let percent = load * 100.0;
    if percent < 0.05 {
        "<0.1% load".to_string()
    } else if percent >= 9.95 {
        format!("{percent:.0}% load")
    } else {
        format!("{percent:.1}% load")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_reads_as_percent_of_one_core() {
        assert_eq!(load_text(0.012), "1.2% load");
        assert_eq!(load_text(0.4), "40% load");
        assert_eq!(load_text(0.0001), "<0.1% load");
    }

    #[test]
    fn the_total_is_every_wait_added_up() {
        let latency = Latency::new(10.0, 52.0, 10.0);
        assert_eq!(latency.total_ms(), 10.0 + 10.0 + 52.0 + 50.0 + 10.0);
        assert_eq!(latency.parts().iter().map(|(_, ms)| ms).sum::<f32>(), latency.total_ms());
    }

    #[test]
    fn with_no_stages_the_fixed_waits_remain() {
        let latency = Latency::new(0.0, 0.0, 0.0);
        assert_eq!(latency.total_ms(), (BLOCK_MS + CUSHION_MS) as f32);
    }

    #[test]
    fn the_breakdown_names_every_part_in_ms() {
        let text = Latency::new(10.0, 3.0, 20.0).breakdown();
        for line in ["Mic buffer: 10 ms", "Processing block: 10 ms", "Stage delays: 3 ms", "Playout cushion: 50 ms", "Output buffer: 20 ms"] {
            assert!(text.contains(line), "{text}");
        }
    }
}
