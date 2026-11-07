//! Parameters shared between the UI thread and the DSP thread. Everything is an atomic,
//! so the audio path never takes a lock.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

/// `%APPDATA%\middlemic\settings.ini`, or the current directory if APPDATA is unset.
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("middlemic").join("settings.ini")
}

pub struct Param {
    pub name: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub log: bool,
    value: AtomicU32,
}

impl Param {
    pub fn new(name: &'static str, unit: &'static str, min: f32, max: f32, default: f32) -> Self {
        Self {
            name,
            unit,
            min,
            max,
            default,
            log: false,
            value: AtomicU32::new(default.to_bits()),
        }
    }

    /// Slider moves logarithmically (frequencies).
    pub fn log(mut self) -> Self {
        self.log = true;
        self
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.value.load(Relaxed))
    }

    pub fn set(&self, v: f32) {
        self.value.store(v.clamp(self.min, self.max).to_bits(), Relaxed);
    }

    pub fn reset(&self) {
        self.set(self.default);
    }

    /// Position of `value` along the control, `0..=1` (logarithmic for frequencies).
    pub fn to_norm(&self, value: f32) -> f32 {
        let n = if self.log {
            (value.max(self.min).ln() - self.min.ln()) / (self.max.ln() - self.min.ln())
        } else {
            (value - self.min) / (self.max - self.min)
        };
        n.clamp(0.0, 1.0)
    }

    pub fn from_norm(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        if self.log {
            (self.min.ln() + n * (self.max.ln() - self.min.ln())).exp()
        } else {
            self.min + n * (self.max - self.min)
        }
    }

    /// Short text for a knob: "1.20 kHz", "+3.0 dB", "50 %".
    pub fn display(&self) -> String {
        let v = self.get();
        match self.unit {
            "Hz" if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            "Hz" => format!("{v:.0} Hz"),
            "dB" if self.min < 0.0 => format!("{v:+.1} dB"),
            "" => format!("{v:.1}"),
            unit => format!("{v:.1} {unit}"),
        }
    }
}

/// One stage's controls: an on/off switch and its parameters.
pub struct Section {
    pub name: &'static str,
    pub enabled: AtomicBool,
    pub params: Vec<Param>,
    /// One label per gain-reduction meter this stage reports (empty for most stages).
    pub reduction_labels: Vec<&'static str>,
    reduction: Vec<AtomicU32>,
    /// Few-dial alternative to `params`, for stages with a Simple mode. The stage derives
    /// its settings from these until `advanced` is switched on.
    pub simple: Vec<Param>,
    /// Buttons that set the first simple parameter, e.g. ("Gentle", 25.0).
    pub quick_picks: Vec<(&'static str, f32)>,
    pub advanced: AtomicBool,
    /// Whether the stage is unfolded in the rack. Saved with the other settings.
    pub open: AtomicBool,
}

impl Section {
    pub fn new(name: &'static str, params: Vec<Param>) -> Self {
        Self {
            name,
            enabled: AtomicBool::new(true),
            params,
            reduction_labels: Vec::new(),
            reduction: Vec::new(),
            simple: Vec::new(),
            quick_picks: Vec::new(),
            advanced: AtomicBool::new(false),
            open: AtomicBool::new(false),
        }
    }

    /// Start unfolded in the rack (stages are folded by default).
    pub fn unfolded(self) -> Self {
        self.open.store(true, Relaxed);
        self
    }

    /// Give the stage a Simple mode. It starts in Simple; `params` become the Advanced view.
    pub fn with_simple(mut self, simple: Vec<Param>, quick_picks: &[(&'static str, f32)]) -> Self {
        self.simple = simple;
        self.quick_picks = quick_picks.to_vec();
        self
    }

    /// True when the stage should read `params`. Stages without a Simple mode are always advanced.
    pub fn is_advanced(&self) -> bool {
        self.simple.is_empty() || self.advanced.load(Relaxed)
    }

    /// Give the stage gain-reduction meters, one per label.
    pub fn with_reduction_meters(mut self, labels: &[&'static str]) -> Self {
        self.reduction_labels = labels.to_vec();
        self.reduction = labels.iter().map(|_| AtomicU32::new(0)).collect();
        self
    }

    /// Keeps the largest reduction (dB, positive) since the UI last looked.
    pub fn record_reduction(&self, slot: usize, db: f32) {
        self.reduction[slot].fetch_max(db.max(0.0).to_bits(), Relaxed);
    }

    pub fn take_reduction(&self, slot: usize) -> f32 {
        f32::from_bits(self.reduction[slot].swap(0, Relaxed))
    }

    pub fn get(&self, index: usize) -> f32 {
        self.params[index].get()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }

    pub fn reset(&self) {
        self.enabled.store(true, Relaxed);
        self.advanced.store(false, Relaxed);
        self.params.iter().chain(&self.simple).for_each(Param::reset);
    }
}

/// The most recent audio, for the spectrum display. The DSP thread writes, the UI reads
/// without locking. A read that overlaps a write may mix old and new samples, which is
/// harmless for drawing.
pub struct Scope {
    samples: Vec<AtomicU32>,
    written: AtomicUsize,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            samples: (0..Self::LEN).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicUsize::new(0),
        }
    }
}

impl Scope {
    pub const LEN: usize = 4096;

    pub fn write(&self, chunk: &[f32]) {
        let mut at = self.written.load(Relaxed);
        for s in chunk {
            self.samples[at % Self::LEN].store(s.to_bits(), Relaxed);
            at += 1;
        }
        self.written.store(at, Relaxed);
    }

    /// Fills `out` with the latest samples, oldest first. `out.len()` must be `<= LEN`.
    pub fn read_latest(&self, out: &mut [f32]) {
        let start = self.written.load(Relaxed) + Self::LEN - out.len();
        for (i, o) in out.iter_mut().enumerate() {
            *o = f32::from_bits(self.samples[(start + i) % Self::LEN].load(Relaxed));
        }
    }
}

/// Levels written by the DSP thread, read by the UI. Peaks are linear, `>= 0`, so the
/// raw `f32` bits order the same as the values and `fetch_max` works.
#[derive(Default)]
pub struct Meters {
    input_peak: AtomicU32,
    output_peak: AtomicU32,
    load: AtomicU32,
    latency_ms: AtomicU32,
    dropouts: AtomicU32,
    pub input_scope: Scope,
    pub output_scope: Scope,
}

impl Meters {
    pub fn record_input(&self, peak: f32) {
        self.input_peak.fetch_max(peak.to_bits(), Relaxed);
    }

    pub fn record_output(&self, peak: f32) {
        self.output_peak.fetch_max(peak.to_bits(), Relaxed);
    }

    pub fn set_load(&self, load: f32) {
        self.load.store(load.to_bits(), Relaxed);
    }

    /// An audible gap: the output ran dry, or audio had to be thrown away to catch up.
    pub fn record_dropout(&self) {
        self.dropouts.fetch_add(1, Relaxed);
    }

    pub fn dropouts(&self) -> u32 {
        self.dropouts.load(Relaxed)
    }

    pub fn clear_dropouts(&self) {
        self.dropouts.store(0, Relaxed);
    }

    pub fn set_latency_ms(&self, ms: f32) {
        self.latency_ms.store(ms.to_bits(), Relaxed);
    }

    /// Delay added by the active stages. Excludes device buffers.
    pub fn latency_ms(&self) -> f32 {
        f32::from_bits(self.latency_ms.load(Relaxed))
    }

    /// Peak since the last call.
    pub fn take_input(&self) -> f32 {
        f32::from_bits(self.input_peak.swap(0, Relaxed))
    }

    pub fn take_output(&self) -> f32 {
        f32::from_bits(self.output_peak.swap(0, Relaxed))
    }

    /// Fraction of the real-time budget the DSP thread is using.
    pub fn load(&self) -> f32 {
        f32::from_bits(self.load.load(Relaxed))
    }
}

pub struct Settings {
    pub sections: Vec<Arc<Section>>,
    pub meters: Meters,
}

impl Settings {
    pub fn new(sections: Vec<Section>) -> Arc<Self> {
        Arc::new(Self {
            sections: sections.into_iter().map(Arc::new).collect(),
            meters: Meters::default(),
        })
    }

    pub fn section(&self, name: &str) -> Arc<Section> {
        self.sections
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no section named {name:?}"))
            .clone()
    }

    pub fn reset(&self) {
        self.sections.iter().for_each(|s| s.reset());
    }

    /// INI-style text. `extras` are written before the first section.
    pub fn to_text(&self, extras: &[(&str, &str)]) -> String {
        let mut out = String::new();
        for (key, value) in extras {
            out.push_str(&format!("{key}={value}\n"));
        }
        for section in &self.sections {
            out.push_str(&format!(
                "\n[{}]\nenabled={}\nopen={}\n",
                section.name,
                section.is_enabled(),
                section.open.load(Relaxed)
            ));
            if !section.simple.is_empty() {
                out.push_str(&format!("advanced={}\n", section.advanced.load(Relaxed)));
            }
            for p in section.simple.iter().chain(&section.params) {
                out.push_str(&format!("{}={}\n", p.name, p.get()));
            }
        }
        out
    }

    /// Apply saved text. Unknown or malformed lines are ignored. Returns the extras.
    pub fn apply_text(&self, text: &str) -> HashMap<String, String> {
        let mut extras = HashMap::new();
        let mut current: Option<&Arc<Section>> = None;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = self.sections.iter().find(|s| s.name == name);
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match current {
                None => {
                    extras.insert(key.to_string(), value.to_string());
                }
                Some(section) if key == "enabled" => {
                    if let Ok(v) = value.parse() {
                        section.enabled.store(v, Relaxed);
                    }
                }
                Some(section) if key == "open" => {
                    if let Ok(v) = value.parse() {
                        section.open.store(v, Relaxed);
                    }
                }
                Some(section) if key == "advanced" => {
                    if let Ok(v) = value.parse() {
                        section.advanced.store(v, Relaxed);
                    }
                }
                Some(section) => {
                    let found = section.simple.iter().chain(&section.params).find(|p| p.name == key);
                    if let (Some(p), Ok(v)) = (found, value.parse()) {
                        p.set(v);
                    }
                }
            }
        }
        extras
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Arc<Settings> {
        Settings::new(vec![Section::new(
            "EQ",
            vec![Param::new("Boost", "dB", 0.0, 30.0, 5.0)],
        )])
    }

    #[test]
    fn set_clamps_to_range() {
        let p = Param::new("x", "", 0.0, 10.0, 1.0);
        p.set(99.0);
        assert_eq!(p.get(), 10.0);
    }

    #[test]
    fn text_round_trips_values_and_extras() {
        let a = settings();
        a.section("EQ").params[0].set(12.5);
        a.section("EQ").enabled.store(false, Relaxed);
        let text = a.to_text(&[("device.output", "CABLE Input")]);

        let b = settings();
        let extras = b.apply_text(&text);
        assert_eq!(b.section("EQ").params[0].get(), 12.5);
        assert!(!b.section("EQ").is_enabled());
        assert_eq!(extras["device.output"], "CABLE Input");
    }

    #[test]
    fn junk_in_saved_text_is_ignored() {
        let s = settings();
        s.apply_text("[Nope]\nBoost=9\n[EQ]\nBoost=abc\nwhat\n");
        assert_eq!(s.section("EQ").params[0].get(), 5.0);
    }

    #[test]
    fn simple_mode_round_trips_and_resets() {
        let make = || {
            Settings::new(vec![
                Section::new("C", vec![Param::new("Ratio", ":1", 1.0, 10.0, 2.0)])
                    .with_simple(vec![Param::new("Amount", "%", 0.0, 100.0, 50.0)], &[("Gentle", 25.0)]),
            ])
        };
        let a = make();
        assert!(!a.section("C").is_advanced(), "starts in simple mode");
        a.section("C").simple[0].set(80.0);
        a.section("C").advanced.store(true, Relaxed);
        a.section("C").params[0].set(6.0);

        let b = make();
        b.apply_text(&a.to_text(&[]));
        assert!(b.section("C").is_advanced());
        assert_eq!(b.section("C").simple[0].get(), 80.0);
        assert_eq!(b.section("C").params[0].get(), 6.0);

        b.reset();
        assert!(!b.section("C").is_advanced());
        assert_eq!(b.section("C").simple[0].get(), 50.0);
    }

    #[test]
    fn norm_round_trips_for_linear_and_log() {
        let lin = Param::new("g", "dB", -12.0, 12.0, 0.0);
        let log = Param::new("f", "Hz", 20.0, 20_000.0, 1_000.0).log();
        assert!((lin.to_norm(0.0) - 0.5).abs() < 1e-6);
        assert!((log.to_norm(632.455) - 0.5).abs() < 1e-3, "log midpoint is the geometric mean");
        for n in [0.0, 0.2, 0.5, 0.9, 1.0] {
            assert!((lin.to_norm(lin.from_norm(n)) - n).abs() < 1e-5);
            assert!((log.to_norm(log.from_norm(n)) - n).abs() < 1e-5);
        }
    }

    #[test]
    fn display_reads_naturally() {
        let f = Param::new("f", "Hz", 20.0, 20_000.0, 80.0);
        assert_eq!(f.display(), "80 Hz");
        f.set(1_250.0);
        assert_eq!(f.display(), "1.25 kHz");
        let g = Param::new("g", "dB", -12.0, 12.0, 3.0);
        assert_eq!(g.display(), "+3.0 dB");
        assert_eq!(Param::new("a", "%", 0.0, 100.0, 50.0).display(), "50.0 %");
    }

    #[test]
    fn scope_returns_the_latest_samples_in_order_across_wraparound() {
        let s = Scope::default();
        s.write(&(0..10).map(|i| i as f32).collect::<Vec<_>>());
        let mut out = [0.0; 4];
        s.read_latest(&mut out);
        assert_eq!(out, [6.0, 7.0, 8.0, 9.0]);

        let long: Vec<f32> = (0..Scope::LEN + 100).map(|i| i as f32).collect();
        s.write(&long);
        let mut out = [0.0; 3];
        s.read_latest(&mut out);
        let last = (Scope::LEN + 99) as f32;
        assert_eq!(out, [last - 2.0, last - 1.0, last]);
    }

    #[test]
    fn fold_state_is_remembered_but_not_touched_by_reset() {
        let make = || Settings::new(vec![Section::new("EQ", vec![]).unfolded(), Section::new("Gate", vec![])]);
        let a = make();
        assert!(a.section("EQ").open.load(Relaxed) && !a.section("Gate").open.load(Relaxed));
        a.section("EQ").open.store(false, Relaxed);
        a.section("Gate").open.store(true, Relaxed);

        let b = make();
        b.apply_text(&a.to_text(&[]));
        assert!(!b.section("EQ").open.load(Relaxed), "saved fold wins over the default");
        assert!(b.section("Gate").open.load(Relaxed));

        b.reset();
        assert!(b.section("Gate").open.load(Relaxed), "Reset is about sound, not layout");
    }

    #[test]
    fn stages_without_simple_mode_are_always_advanced() {
        assert!(Section::new("EQ", vec![]).is_advanced());
    }

    #[test]
    fn reduction_meter_keeps_largest_until_read() {
        let s = Section::new("C", vec![]).with_reduction_meters(&["Low", "High"]);
        s.record_reduction(1, 2.0);
        s.record_reduction(1, 7.5);
        s.record_reduction(1, -3.0);
        assert_eq!(s.take_reduction(1), 7.5);
        assert_eq!(s.take_reduction(1), 0.0);
        assert_eq!(s.take_reduction(0), 0.0);
    }

    #[test]
    fn meter_keeps_largest_peak_until_read() {
        let m = Meters::default();
        m.record_input(0.2);
        m.record_input(0.7);
        m.record_input(0.1);
        assert_eq!(m.take_input(), 0.7);
        assert_eq!(m.take_input(), 0.0);
    }
}
