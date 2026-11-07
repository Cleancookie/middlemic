//! Parameters shared between the UI thread and the DSP thread. Everything is an atomic,
//! so the audio path never takes a lock.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

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
        }
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

/// Levels written by the DSP thread, read by the UI. Peaks are linear, `>= 0`, so the
/// raw `f32` bits order the same as the values and `fetch_max` works.
#[derive(Default)]
pub struct Meters {
    input_peak: AtomicU32,
    output_peak: AtomicU32,
    load: AtomicU32,
    latency_ms: AtomicU32,
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
            out.push_str(&format!("\n[{}]\nenabled={}\n", section.name, section.is_enabled()));
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
