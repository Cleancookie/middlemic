//! Parameters shared between the UI thread and the DSP thread. Controls are atomics, so the
//! audio path never takes a lock for them. The list of stages sits behind a mutex that the
//! DSP thread only takes when the rack's generation says it changed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::{Arc, Mutex};

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
    /// Plain-language tooltip body: what it does and where to start. Empty shows only the name.
    pub help: &'static str,
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
            help: "",
            value: AtomicU32::new(default.to_bits()),
        }
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
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
            "1:" => format!("1:{v:.1}"),
            "notches" => format!("{v:.0}"),
            "" => format!("{v:.1}"),
            unit => format!("{v:.1} {unit}"),
        }
    }
}

/// Non-negative floats order like their bits, so `fetch_max` keeps the largest.
fn keep_max(slot: &AtomicU32, value: f32) {
    slot.fetch_max(value.max(0.0).to_bits(), Relaxed);
}

fn take(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.swap(0, Relaxed))
}

/// One stage's controls: an on/off switch and its parameters.
pub struct Section {
    pub name: &'static str,
    pub enabled: AtomicBool,
    default_enabled: bool,
    pub params: Vec<Param>,
    /// One label per gain-reduction meter this stage reports (empty for most stages).
    pub reduction_labels: Vec<&'static str>,
    reduction: Vec<AtomicU32>,
    /// Peak levels (linear) the stage reports for its own display, by slot.
    levels: Vec<AtomicU32>,
    /// Values the stage shows as they are now, such as the Feedback suppressor's notches.
    readouts: Vec<AtomicU32>,
    /// Set by the UI to ask the stage to forget what it learned; the stage takes it.
    clear: AtomicBool,
    /// Names saved by older versions, each with the parameters it now sets.
    renamed: Vec<(&'static str, &'static [&'static str])>,
    /// Few-dial alternative to `params`, for stages with a Simple mode. The stage derives
    /// its settings from these until `advanced` is switched on.
    pub simple: Vec<Param>,
    /// Buttons that set the first simple parameter, with their tooltip, e.g. ("Gentle", 25.0, "...").
    pub quick_picks: Vec<(&'static str, f32, &'static str)>,
    /// One line on what the stage is for, shown in the + menu and on the row's name.
    pub help: &'static str,
    pub advanced: AtomicBool,
    /// Whether the stage is unfolded in the rack. Saved with the other settings.
    pub open: AtomicBool,
    /// A problem the stage wants the user to see, such as a missing model file. Empty when fine.
    status: std::sync::Mutex<String>,
    /// Delay this stage adds right now, in ms (`f32` bits). Written by the DSP thread.
    latency_ms: AtomicU32,
    /// Share of real time this stage's processing takes, as an `f32` fraction (1.0 is one
    /// whole core). Written by the DSP thread.
    load: AtomicU32,
}

impl Section {
    pub fn new(name: &'static str, params: Vec<Param>) -> Self {
        Self {
            name,
            enabled: AtomicBool::new(true),
            default_enabled: true,
            params,
            reduction_labels: Vec::new(),
            reduction: Vec::new(),
            levels: Vec::new(),
            readouts: Vec::new(),
            clear: AtomicBool::new(false),
            renamed: Vec::new(),
            simple: Vec::new(),
            quick_picks: Vec::new(),
            help: "",
            advanced: AtomicBool::new(false),
            open: AtomicBool::new(false),
            status: std::sync::Mutex::new(String::new()),
            latency_ms: AtomicU32::new(0),
            load: AtomicU32::new(0),
        }
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.load.load(Relaxed))
    }

    pub fn latency_ms(&self) -> f32 {
        f32::from_bits(self.latency_ms.load(Relaxed))
    }

    pub fn set_latency_ms(&self, ms: f32) {
        self.latency_ms.store(ms.to_bits(), Relaxed);
    }

    pub fn set_load(&self, load: f32) {
        self.load.store(load.to_bits(), Relaxed);
    }

    pub fn status(&self) -> String {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Show `message` in the stage's row, unfolding it the first time so it is seen (not again,
    /// or a row the user folded would keep springing open). An empty message clears it.
    pub fn set_status(&self, message: impl Into<String>) {
        let message = message.into();
        if let Ok(mut status) = self.status.lock() {
            if !message.is_empty() && *status != message {
                self.open.store(true, Relaxed);
            }
            *status = message;
        }
    }

    /// Start switched off, and return to off on reset.
    pub fn off_by_default(mut self) -> Self {
        self.default_enabled = false;
        self.enabled.store(false, Relaxed);
        self
    }

    /// Start unfolded in the rack (stages are folded by default).
    pub fn unfolded(self) -> Self {
        self.open.store(true, Relaxed);
        self
    }

    /// Give the stage a Simple mode. It starts in Simple; `params` become the Advanced view.
    pub fn with_simple(mut self, simple: Vec<Param>, quick_picks: &[(&'static str, f32, &'static str)]) -> Self {
        self.simple = simple;
        self.quick_picks = quick_picks.to_vec();
        self
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = help;
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
        keep_max(&self.reduction[slot], db);
    }

    pub fn take_reduction(&self, slot: usize) -> f32 {
        take(&self.reduction[slot])
    }

    /// Give the stage `count` level meters.
    pub fn with_level_meters(mut self, count: usize) -> Self {
        self.levels = (0..count).map(|_| AtomicU32::new(0)).collect();
        self
    }

    /// Keeps the loudest peak (linear) since the UI last looked.
    pub fn record_level(&self, slot: usize, peak: f32) {
        keep_max(&self.levels[slot], peak);
    }

    pub fn take_level(&self, slot: usize) -> f32 {
        take(&self.levels[slot])
    }

    pub fn level_meters(&self) -> usize {
        self.levels.len()
    }

    /// Give the stage `count` readouts, all starting at 0.
    pub fn with_readouts(mut self, count: usize) -> Self {
        self.readouts = (0..count).map(|_| AtomicU32::new(0)).collect();
        self
    }

    pub fn set_readout(&self, slot: usize, value: f32) {
        self.readouts[slot].store(value.to_bits(), Relaxed);
    }

    pub fn readout(&self, slot: usize) -> f32 {
        f32::from_bits(self.readouts[slot].load(Relaxed))
    }

    pub fn request_clear(&self) {
        self.clear.store(true, Relaxed);
    }

    pub fn take_clear_request(&self) -> bool {
        self.clear.swap(false, Relaxed)
    }

    /// Load values saved under `old` names into the parameters that replaced them.
    pub fn with_renamed(mut self, renamed: &[(&'static str, &'static [&'static str])]) -> Self {
        self.renamed = renamed.to_vec();
        self
    }

    pub fn get(&self, index: usize) -> f32 {
        self.params[index].get()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }

    pub fn reset(&self) {
        self.enabled.store(self.default_enabled, Relaxed);
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
    capture_buffer_ms: AtomicU32,
    render_buffer_ms: AtomicU32,
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

    pub fn set_capture_buffer_ms(&self, ms: f32) {
        self.capture_buffer_ms.store(ms.to_bits(), Relaxed);
    }

    pub fn set_render_buffer_ms(&self, ms: f32) {
        self.render_buffer_ms.store(ms.to_bits(), Relaxed);
    }

    /// The input device's period, as reported when the engine opened it.
    pub fn capture_buffer_ms(&self) -> f32 {
        f32::from_bits(self.capture_buffer_ms.load(Relaxed))
    }

    /// The output device's buffer, as reported when the engine opened it.
    pub fn render_buffer_ms(&self) -> f32 {
        f32::from_bits(self.render_buffer_ms.load(Relaxed))
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

/// `list` with the entry at position `from` moved into the gap before position `gap`
/// (`0..=len`, where `len` means after the last).
pub fn moved<T: Clone>(list: &[T], from: usize, gap: usize) -> Vec<T> {
    let mut out = list.to_vec();
    let item = out.remove(from);
    let at = if gap > from { gap - 1 } else { gap };
    out.insert(at.min(out.len()), item);
    out
}

/// One stage in the rack: an instance of a kind (its section's name) with its own controls.
pub struct Item {
    /// Unique within the rack and saved with it, so the DSP thread can tell new stages from old.
    pub id: u64,
    /// The kind's name, numbered from the second of a kind on: "EQ", "EQ 2".
    pub name: String,
    pub section: Arc<Section>,
}

impl Item {
    pub fn kind(&self) -> &'static str {
        self.section.name
    }
}

/// The lowest-numbered name not yet taken by a stage of `kind`.
pub fn free_name(items: &[Arc<Item>], kind: &str) -> String {
    let taken = |name: &str| items.iter().any(|i| i.name == name);
    if !taken(kind) {
        return kind.to_string();
    }
    (2..).map(|n| format!("{kind} {n}")).find(|n| !taken(n)).expect("some number is free")
}

/// A removed stage and where it was, so it can be put back.
pub struct Removed {
    pub position: usize,
    pub item: Arc<Item>,
}

/// Marks text written by [`rack_to_text`], which numbers its stages; older files name them.
const FORMAT_LINE: &str = "rack.format=2";

/// The whole rack as text: one `[stage <id>]` block per item, in running order.
pub fn rack_to_text(items: &[Arc<Item>]) -> String {
    let mut out = format!("{FORMAT_LINE}\n");
    for item in items {
        let section = &item.section;
        out.push_str(&format!(
            "\n[stage {}]\nkind={}\nname={}\nenabled={}\nopen={}\n",
            item.id,
            item.kind(),
            item.name,
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

/// The items in text from [`rack_to_text`], in order, or `None` if it is not in that format.
/// Unknown kinds, repeated ids and malformed lines are skipped.
pub fn rack_from_text(text: &str) -> Option<Vec<Item>> {
    let (top, blocks) = parse_ini(text);
    if !top.iter().any(|&(k, v)| format!("{k}={v}") == FORMAT_LINE) {
        return None;
    }
    let mut items: Vec<Item> = Vec::new();
    for (header, lines) in blocks {
        let Some(id) = header.strip_prefix("stage ").and_then(|id| id.trim().parse().ok()) else {
            continue;
        };
        let value = |key| lines.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let Some(section) = value("kind").and_then(crate::dsp::new_section) else {
            continue;
        };
        if items.iter().any(|i| i.id == id) {
            continue;
        }
        lines.iter().for_each(|(k, v)| apply_key(&section, k, v));
        let name = value("name").filter(|n| !n.is_empty()).unwrap_or(section.name).to_string();
        items.push(Item { id, name, section: Arc::new(section) });
    }
    Some(items)
}

type IniLines<'a> = Vec<(&'a str, &'a str)>;

/// `key=value` lines before the first `[header]`, then each header with its lines.
fn parse_ini(text: &str) -> (IniLines<'_>, Vec<(&str, IniLines<'_>)>) {
    let (mut top, mut blocks): (IniLines, Vec<(&str, IniLines)>) = (Vec::new(), Vec::new());
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            blocks.push((header, Vec::new()));
        } else if let Some(pair) = line.split_once('=') {
            match blocks.last_mut() {
                Some((_, lines)) => lines.push(pair),
                None => top.push(pair),
            }
        }
    }
    (top, blocks)
}

/// One saved control. Keys that are not a flag or parameter, and bad values, are ignored.
fn apply_key(section: &Section, key: &str, value: &str) {
    let flag = match key {
        "enabled" => &section.enabled,
        "open" => &section.open,
        "advanced" => &section.advanced,
        _ => {
            let Ok(v) = value.parse() else { return };
            let key = [key];
            let renamed = section.renamed.iter().find(|(old, _)| *old == key[0]);
            let names = renamed.map_or(&key[..], |(_, new)| new);
            for p in section.simple.iter().chain(&section.params).filter(|p| names.contains(&p.name)) {
                p.set(v);
            }
            return;
        }
    };
    if let Ok(v) = value.parse() {
        flag.store(v, Relaxed);
    }
}

struct Rack {
    /// In running order.
    items: Vec<Arc<Item>>,
    next_id: u64,
}

pub struct Settings {
    rack: Mutex<Rack>,
    /// Changes whenever stages are added, removed or moved, so the DSP thread can cheaply notice.
    generation: AtomicU64,
    pub meters: Meters,
}

impl Settings {
    /// A rack of `sections`, in that order.
    pub fn new(sections: Vec<Section>) -> Arc<Self> {
        let settings = Arc::new(Self {
            rack: Mutex::new(Rack { items: Vec::new(), next_id: 1 }),
            generation: AtomicU64::new(0),
            meters: Meters::default(),
        });
        for section in sections {
            settings.push(section);
        }
        settings
    }

    /// Change the rack under its lock. `change` returns whether it changed anything.
    fn edit<R>(&self, change: impl FnOnce(&mut Rack) -> (bool, R)) -> R {
        let mut rack = self.rack.lock().unwrap();
        let (changed, result) = change(&mut rack);
        if changed {
            self.generation.fetch_add(1, Release);
        }
        result
    }

    fn push(&self, section: Section) -> Arc<Item> {
        self.edit(|rack| {
            let item = Arc::new(Item {
                id: rack.next_id,
                name: free_name(&rack.items, section.name),
                section: Arc::new(section),
            });
            rack.next_id += 1;
            rack.items.push(item.clone());
            (true, item)
        })
    }

    /// The stages in running order.
    pub fn items(&self) -> Vec<Arc<Item>> {
        self.rack.lock().unwrap().items.clone()
    }

    /// Publish each stage's delay (ms) and load to its row. Ids not in the rack are skipped.
    pub fn set_stage_stats(&self, stats: impl Iterator<Item = (u64, f32, f32)>) {
        let rack = self.rack.lock().unwrap();
        for (id, ms, load) in stats {
            if let Some(item) = rack.items.iter().find(|i| i.id == id) {
                item.section.set_latency_ms(ms);
                item.section.set_load(load);
            }
        }
    }

    /// Back to zero, for when the audio stops and no stage adds any delay or load.
    pub fn clear_stage_stats(&self) {
        for item in self.items() {
            item.section.set_latency_ms(0.0);
            item.section.set_load(0.0);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Acquire)
    }

    /// The generation together with the stages it describes.
    pub fn snapshot(&self) -> (u64, Vec<Arc<Item>>) {
        let rack = self.rack.lock().unwrap();
        (self.generation(), rack.items.clone())
    }

    /// Append a new stage, unfolded.
    pub fn add(&self, section: Section) -> Arc<Item> {
        self.push(section.unfolded())
    }

    pub fn remove(&self, id: u64) -> Option<Removed> {
        self.edit(|rack| {
            let Some(position) = rack.items.iter().position(|i| i.id == id) else {
                return (false, None);
            };
            let item = rack.items.remove(position);
            (true, Some(Removed { position, item }))
        })
    }

    /// Put a removed stage back where it was, renumbered if its name was taken meanwhile.
    pub fn restore(&self, removed: Removed) {
        self.edit(|rack| {
            if rack.items.iter().any(|i| i.id == removed.item.id) {
                return (false, ());
            }
            let mut item = removed.item;
            if rack.items.iter().any(|i| i.name == item.name) {
                item = Arc::new(Item {
                    id: item.id,
                    name: free_name(&rack.items, item.kind()),
                    section: item.section.clone(),
                });
            }
            let at = removed.position.min(rack.items.len());
            rack.items.insert(at, item);
            (true, ())
        })
    }

    /// Move the stage at position `from` into the gap before position `gap` (see [`moved`]).
    pub fn move_item(&self, from: usize, gap: usize) {
        self.edit(|rack| {
            if from >= rack.items.len() {
                return (false, ());
            }
            let items = moved(&rack.items, from, gap);
            let changed = items.iter().zip(&rack.items).any(|(a, b)| a.id != b.id);
            rack.items = items;
            (changed, ())
        })
    }

    /// Replace every stage, keeping new ids clear of the ones given.
    fn replace(&self, items: Vec<Item>) {
        self.edit(|rack| {
            let highest = items.iter().map(|i| i.id).max().unwrap_or(0);
            rack.next_id = rack.next_id.max(highest + 1);
            rack.items = items.into_iter().map(Arc::new).collect();
            (true, ())
        })
    }

    /// Replace the rack with the one in `text` (see [`rack_from_text`]). False, and nothing
    /// changed, if the text is not in that format.
    pub fn load_rack(&self, text: &str) -> bool {
        let Some(items) = rack_from_text(text) else {
            return false;
        };
        self.replace(items);
        true
    }

    /// Apply an order saved by name, from before stages had ids. Unknown names are skipped and
    /// stages missing from the list go back to their usual position, so a stage added in a
    /// later version (echo cancellation, which must come first) is not exiled to the end.
    fn set_order_by_names(&self, names: &str) {
        self.edit(|rack| {
            let mut order: Vec<usize> = Vec::new();
            for name in names.split(',').map(str::trim) {
                if let Some(i) = rack.items.iter().position(|s| s.name == name) {
                    if !order.contains(&i) {
                        order.push(i);
                    }
                }
            }
            for i in 0..rack.items.len() {
                if !order.contains(&i) {
                    order.insert(i.min(order.len()), i);
                }
            }
            let changed = order.iter().enumerate().any(|(at, &i)| at != i);
            rack.items = order.iter().map(|&i| rack.items[i].clone()).collect();
            (changed, ())
        })
    }

    /// The controls of the stage called `name`. Panics if there is none.
    #[cfg(test)]
    pub fn section(&self, name: &str) -> Arc<Section> {
        self.items()
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("no stage named {name:?}"))
            .section
            .clone()
    }

    /// Every stage back to its default sound. Which stages there are, and their order, stay.
    pub fn reset(&self) {
        self.items().iter().for_each(|i| i.section.reset());
    }

    /// INI-style text: `extras`, then the rack.
    pub fn to_text(&self, extras: &[(&str, &str)]) -> String {
        let mut out: String = extras.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        out.push_str(&rack_to_text(&self.items()));
        out
    }

    /// Apply saved text. Unknown or malformed lines are ignored. Returns the extras.
    /// A file from before stages had ids (`[EQ]` blocks) is applied to the stages of those
    /// names, which for a fresh rack are the defaults, one of each kind.
    pub fn apply_text(&self, text: &str) -> HashMap<String, String> {
        let (top, blocks) = parse_ini(text);
        let mut extras = HashMap::new();
        let mut old_order = None;
        for (key, value) in top {
            match key {
                "order" => old_order = Some(value),
                "rack.format" => {}
                _ => {
                    extras.insert(key.to_string(), value.to_string());
                }
            }
        }
        if let Some(items) = rack_from_text(text) {
            self.replace(items);
            return extras;
        }
        let items = self.items();
        for (name, lines) in blocks {
            if let Some(item) = items.iter().find(|i| i.name == name) {
                lines.iter().for_each(|(k, v)| apply_key(&item.section, k, v));
            }
        }
        if let Some(names) = old_order {
            self.set_order_by_names(names);
        }
        extras
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_stage_stats_zeroes_latency() {
        let settings = Settings::new(vec![Section::new("A", vec![]), Section::new("B", vec![]), Section::new("C", vec![])]);
        let items = settings.items();
        settings.set_stage_stats(items.iter().map(|i| (i.id, 1.0, 0.25)));
        settings.clear_stage_stats();
        assert_eq!(items[0].section.latency_ms(), 0.0);
    }
    use crate::dsp::new_section;

    fn rack(kinds: &[&str]) -> Arc<Settings> {
        Settings::new(kinds.iter().map(|k| new_section(k).unwrap()).collect())
    }

    fn names(settings: &Settings) -> Vec<String> {
        settings.items().iter().map(|i| i.name.clone()).collect()
    }

    #[test]
    fn set_clamps_to_range() {
        let p = Param::new("x", "", 0.0, 10.0, 1.0);
        p.set(99.0);
        assert_eq!(p.get(), 10.0);
    }

    #[test]
    fn text_round_trips_values_and_extras() {
        let a = rack(&["Mic boost"]);
        a.section("Mic boost").params[0].set(12.5);
        a.section("Mic boost").enabled.store(false, Relaxed);
        let text = a.to_text(&[("device.output", "CABLE Input")]);

        let b = rack(&["Mic boost"]);
        let extras = b.apply_text(&text);
        assert_eq!(b.section("Mic boost").params[0].get(), 12.5);
        assert!(!b.section("Mic boost").is_enabled());
        assert_eq!(extras["device.output"], "CABLE Input");
        assert!(!extras.contains_key("rack.format"));
    }

    #[test]
    fn junk_in_saved_text_is_ignored() {
        let s = rack(&["Mic boost"]);
        s.apply_text("[Nope]\nBoost=9\n[Mic boost]\nBoost=abc\nwhat\n");
        assert_eq!(s.section("Mic boost").params[0].get(), 0.0);

        let items = rack_from_text(&format!(
            "{FORMAT_LINE}\n[stage 4]\nkind=Gone\n[stage x]\nkind=EQ\n[stage 5]\nkind=EQ\n[stage 5]\nkind=Limiter\n[stage 6]\nname=no kind\n"
        ))
        .unwrap();
        let kept: Vec<_> = items.iter().map(|i| (i.id, i.kind())).collect();
        assert_eq!(kept, [(5, "EQ")], "unknown kind, bad id, repeated id and missing kind skipped");
    }

    #[test]
    fn simple_mode_round_trips_and_resets() {
        let a = rack(&["Compressor"]);
        let c = a.section("Compressor");
        assert!(!c.is_advanced(), "starts in simple mode");
        c.simple[0].set(80.0);
        c.advanced.store(true, Relaxed);
        c.params[0].set(600.0);

        let b = rack(&["Compressor"]);
        b.apply_text(&a.to_text(&[]));
        let c = b.section("Compressor");
        assert!(c.is_advanced());
        assert_eq!(c.simple[0].get(), 80.0);
        assert_eq!(c.params[0].get(), 600.0);

        b.reset();
        assert!(!c.is_advanced());
        assert_eq!(c.simple[0].get(), 50.0);
    }

    #[test]
    fn a_second_stage_of_a_kind_takes_the_lowest_free_number() {
        let s = rack(&["EQ", "Limiter"]);
        let eq2 = s.add(new_section("EQ").unwrap());
        let eq3 = s.add(new_section("EQ").unwrap());
        assert_eq!(names(&s), ["EQ", "Limiter", "EQ 2", "EQ 3"]);
        assert!(eq2.section.open.load(Relaxed), "added stages start unfolded");

        let ids: Vec<u64> = s.items().iter().map(|i| i.id).collect();
        assert!(ids.windows(2).all(|w| w[0] != w[1]) && !ids[..2].contains(&eq3.id));

        s.remove(eq2.id).unwrap();
        let first = s.items()[0].id;
        s.remove(first).unwrap();
        assert_eq!(names(&s), ["Limiter", "EQ 3"]);
        s.add(new_section("EQ").unwrap());
        s.add(new_section("EQ").unwrap());
        assert_eq!(names(&s), ["Limiter", "EQ 3", "EQ", "EQ 2"]);
        assert!(s.remove(999).is_none());
    }

    #[test]
    fn undo_puts_a_removed_stage_back_where_it_was() {
        let s = rack(&["Mic boost", "EQ", "Limiter"]);
        let eq = s.items()[1].clone();
        eq.section.params[1].set(321.0);
        let removed = s.remove(eq.id).unwrap();
        assert_eq!(names(&s), ["Mic boost", "Limiter"]);

        let generation = s.generation();
        s.restore(removed);
        assert_ne!(s.generation(), generation);
        let back = s.items()[1].clone();
        assert_eq!((back.id, back.name.as_str()), (eq.id, "EQ"));
        assert_eq!(back.section.params[1].get(), 321.0, "with its settings");

        let removed = s.remove(eq.id).unwrap();
        s.add(new_section("EQ").unwrap());
        s.restore(removed);
        assert_eq!(names(&s), ["Mic boost", "EQ 2", "Limiter", "EQ"], "renumbered, as the name was taken");
    }

    #[test]
    fn the_rack_round_trips_with_ids_names_order_and_each_instances_values() {
        let a = rack(&["EQ", "Limiter"]);
        let eq2 = a.add(new_section("EQ").unwrap());
        a.move_item(2, 0);
        eq2.section.params[0].set(300.0);
        eq2.section.enabled.store(false, Relaxed);
        a.items()[1].section.params[0].set(150.0);

        let b = Settings::new(crate::dsp::default_sections());
        b.apply_text(&a.to_text(&[]));
        let (left, right) = (a.items(), b.items());
        let summary = |items: &[Arc<Item>]| -> Vec<_> {
            items
                .iter()
                .map(|i| (i.id, i.name.clone(), i.section.is_enabled(), i.section.open.load(Relaxed), i.section.params[0].get()))
                .collect()
        };
        assert_eq!(summary(&right), summary(&left));
        assert_eq!(names(&b), ["EQ 2", "EQ", "Limiter"]);

        let next = b.add(new_section("EQ").unwrap());
        assert!(left.iter().all(|i| i.id < next.id), "new ids never reuse saved ones");
        assert_eq!(next.name, "EQ 3");
    }

    #[test]
    fn an_empty_rack_stays_empty() {
        let a = rack(&["EQ"]);
        a.remove(a.items()[0].id);
        let b = Settings::new(crate::dsp::default_sections());
        b.apply_text(&a.to_text(&[]));
        assert!(b.items().is_empty());
    }

    /// What the version before numbered stages saved for the default rack.
    const OLD_FILE: &str = "order=Echo cancellation,Noise removal,Noise removal (DPDFNet),Noise removal (RNNoise),Mic boost,De-esser,EQ,Compressor,Limiter
device.input=System default
device.output=CABLE Input (VB-Audio Virtual Cable)

[Echo cancellation]
enabled=true
open=false

[EQ]
enabled=false
open=false
High-pass=120

[Compressor]
enabled=true
open=true
advanced=true
Amount=70
Low crossover=300
";

    #[test]
    fn a_file_from_before_numbered_stages_maps_each_block_onto_one_stage() {
        let s = Settings::new(crate::dsp::default_sections());
        let extras = s.apply_text(OLD_FILE);
        assert_eq!(extras["device.output"], "CABLE Input (VB-Audio Virtual Cable)");
        assert!(!extras.contains_key("order"));
        let kinds: Vec<_> = s.items().iter().map(|i| i.kind()).collect();
        assert_eq!(kinds[0], "Feedback suppressor", "a stage the file never knew keeps its place");
        assert_eq!(kinds[3..6], ["Mic boost", "De-esser", "EQ"], "saved order kept");
        assert_eq!(names(&s).len(), 8);

        assert!(s.section("Echo cancellation").is_enabled());
        let eq = s.section("EQ");
        assert!(!eq.is_enabled() && !eq.open.load(Relaxed));
        assert_eq!(eq.params[0].get(), 120.0);
        let c = s.section("Compressor");
        assert!(c.is_advanced() && c.open.load(Relaxed));
        assert_eq!((c.simple[0].get(), c.params[0].get()), (70.0, 300.0));

        let saved = s.to_text(&[]);
        let t = Settings::new(crate::dsp::default_sections());
        t.apply_text(&saved);
        assert_eq!(names(&t), names(&s), "and saves back in the new format");
        assert!(saved.contains("[stage "), "{saved}");
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

    fn four_stages() -> Arc<Settings> {
        Settings::new(["A", "B", "C", "D"].map(|n| Section::new(n, vec![])).into())
    }

    #[test]
    fn moving_a_stage_lands_it_in_the_gap_that_was_pointed_at() {
        let o = [0, 1, 2, 3];
        assert_eq!(moved(&o, 3, 0), [3, 0, 1, 2], "to the very top");
        assert_eq!(moved(&o, 0, 4), [1, 2, 3, 0], "to the very bottom");
        assert_eq!(moved(&o, 0, 3), [1, 2, 0, 3], "just above the last");
        assert_eq!(moved(&o, 2, 1), [0, 2, 1, 3], "up one");
        for gap in [1, 2] {
            assert_eq!(moved(&o, 1, gap), o, "dropping on itself changes nothing");
        }
    }

    #[test]
    fn moving_changes_the_generation_only_when_the_order_changes() {
        let s = four_stages();
        let v = s.generation();
        s.move_item(1, 2);
        s.move_item(9, 0);
        assert_eq!(s.generation(), v, "dropped on itself, or out of range");
        s.move_item(3, 0);
        assert_eq!(names(&s), ["D", "A", "B", "C"]);
        assert_ne!(s.generation(), v, "readers can tell it changed");
    }

    #[test]
    fn an_old_order_by_name_survives_added_or_removed_stages() {
        let b = four_stages();
        b.apply_text("order=C,A,D,B\n");
        assert_eq!(names(&b), ["C", "A", "D", "B"]);

        // One stage unknown, one missing, one repeated.
        let c = four_stages();
        c.apply_text("order=D,Gone,D,B\n");
        assert_eq!(names(&c), ["A", "D", "C", "B"], "unknown dropped, missing back at their usual positions");
    }

    #[test]
    fn reset_all_keeps_the_stages_and_their_order() {
        let s = rack(&["Mic boost", "EQ"]);
        s.add(new_section("Mic boost").unwrap()).section.params[0].set(9.0);
        s.move_item(2, 0);
        s.reset();
        assert_eq!(names(&s), ["Mic boost 2", "Mic boost", "EQ"]);
        assert_eq!(s.section("Mic boost 2").params[0].get(), 0.0);
    }

    #[test]
    fn fold_state_is_remembered_but_not_touched_by_reset() {
        let make = || rack(&["EQ", "Limiter"]);
        let a = make();
        assert!(a.section("EQ").open.load(Relaxed) && !a.section("Limiter").open.load(Relaxed));
        a.section("EQ").open.store(false, Relaxed);
        a.section("Limiter").open.store(true, Relaxed);

        let b = make();
        b.apply_text(&a.to_text(&[]));
        assert!(!b.section("EQ").open.load(Relaxed), "saved fold wins over the default");
        assert!(b.section("Limiter").open.load(Relaxed));

        b.reset();
        assert!(b.section("Limiter").open.load(Relaxed), "Reset is about sound, not layout");
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
