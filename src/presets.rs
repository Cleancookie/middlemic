//! Presets: the whole rack saved under a name. The built-ins are compiled in; the user's live in
//! `presets\<name>.ini` next to settings.ini. Device choices are not part of a preset.

use crate::settings::{self, Item, Settings, rack_from_text, rack_to_text};
use std::path::PathBuf;
use std::sync::Arc;

const NAME_KEY: &str = "preset.name";
const ALEX: &str = include_str!("../presets/alexs-preset.ini");
pub const DEFAULT: &str = "Default";
pub const ALEX_NAME: &str = "Alex's preset";
pub const BUILTINS: [&str; 2] = [DEFAULT, ALEX_NAME];

pub fn presets_dir() -> PathBuf {
    settings::settings_path().with_file_name("presets")
}

fn builtin_text(name: &str) -> Option<String> {
    match name {
        DEFAULT => Some(Settings::new(crate::dsp::default_sections()).to_text(&[])),
        ALEX_NAME => Some(ALEX.to_string()),
        _ => None,
    }
}

/// A file name that is valid on Windows for any preset name.
pub fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .take(100)
        .collect();
    let mut stem = cleaned.trim_matches([' ', '.']).to_string();
    if stem.is_empty() {
        stem = "preset".into();
    }
    let base = stem.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && base.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        stem.push('_');
    }
    format!("{stem}.ini")
}

/// The rack as text, ignoring stage ids: loading a preset keeps the ids it was saved with, while
/// a rack restored from settings.ini has its own.
fn fingerprint(items: &[Arc<Item>]) -> String {
    let text = rack_to_text(items);
    let lines: Vec<&str> = text.lines().map(|l| if l.starts_with("[stage ") { "[stage]" } else { l }).collect();
    lines.join("\n")
}

fn fingerprint_of_text(text: &str) -> Option<String> {
    let items: Vec<Arc<Item>> = rack_from_text(text)?.into_iter().map(Arc::new).collect();
    Some(fingerprint(&items))
}

fn name_in(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix(NAME_KEY)?.strip_prefix('='))
}

/// The preset list and which preset the rack was last loaded from or saved as.
pub struct Presets {
    dir: PathBuf,
    user: Vec<String>,
    /// Empty when the rack is not from any preset.
    current: String,
    /// What the rack looked like (see [`fingerprint`]) when `current` was loaded or saved.
    baseline: String,
}

impl Presets {
    /// `current` is the remembered preset name; it is dropped if that preset no longer exists.
    pub fn new(dir: PathBuf, current: &str) -> Self {
        let mut presets = Self { dir, user: Vec::new(), current: String::new(), baseline: String::new() };
        presets.refresh();
        if let Some(baseline) = presets.text(current).and_then(|t| fingerprint_of_text(&t)) {
            presets.current = current.to_string();
            presets.baseline = baseline;
        }
        presets
    }

    pub fn user(&self) -> &[String] {
        &self.user
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    /// True if the rack came from a preset the user can overwrite.
    pub fn is_user(&self) -> bool {
        self.user.contains(&self.current)
    }

    pub fn modified(&self, settings: &Settings) -> bool {
        !self.current.is_empty() && fingerprint(&settings.items()) != self.baseline
    }

    /// What the picker shows: the name, with a bullet while the rack differs from it.
    pub fn label(&self, settings: &Settings) -> String {
        match (self.current.as_str(), self.modified(settings)) {
            ("", _) => "Custom".into(),
            (name, true) => format!("{name} •"),
            (name, false) => name.into(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(file_name(name))
    }

    fn text(&self, name: &str) -> Option<String> {
        builtin_text(name).or_else(|| {
            let known = self.user.iter().any(|n| n == name);
            known.then(|| std::fs::read_to_string(self.path(name)).ok()).flatten()
        })
    }

    /// Re-read the user presets from disk.
    pub fn refresh(&mut self) {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ini"))
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let stem = p.file_stem()?.to_string_lossy().into_owned();
                Some(name_in(&text).map_or(stem, str::to_string))
            })
            .filter(|n| !BUILTINS.iter().any(|b| b.eq_ignore_ascii_case(n)))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        self.user = names;
    }

    /// Replace the rack with the preset `name`; the DSP follows live.
    pub fn load(&mut self, settings: &Settings, name: &str) -> Result<(), String> {
        let text = self.text(name).ok_or_else(|| format!("Preset \"{name}\" not found."))?;
        if !settings.load_rack(&text) {
            return Err(format!("Preset \"{name}\" could not be read."));
        }
        self.current = name.to_string();
        self.baseline = fingerprint(&settings.items());
        Ok(())
    }

    /// Overwrite the current user preset with the rack as it is now.
    pub fn save(&mut self, settings: &Settings) -> Result<(), String> {
        if !self.is_user() {
            return Err("Built-in presets cannot be overwritten; use Save as.".into());
        }
        let name = self.current.clone();
        self.write(&name, settings)
    }

    pub fn save_as(&mut self, settings: &Settings, name: &str) -> Result<(), String> {
        let name = self.check_name(name, None)?;
        self.write(&name, settings)
    }

    pub fn rename(&mut self, new: &str) -> Result<(), String> {
        if !self.is_user() {
            return Err("Built-in presets cannot be renamed.".into());
        }
        let new = self.check_name(new, Some(&self.current))?;
        let old_path = self.path(&self.current);
        let text = std::fs::read_to_string(&old_path).map_err(|e| e.to_string())?;
        let body: String = text.lines().filter(|l| !l.starts_with(NAME_KEY)).map(|l| format!("{l}\n")).collect();
        std::fs::write(self.path(&new), format!("{NAME_KEY}={new}\n{body}")).map_err(|e| e.to_string())?;
        if self.path(&new) != old_path {
            let _ = std::fs::remove_file(old_path);
        }
        self.current = new;
        self.refresh();
        Ok(())
    }

    /// Delete the current user preset file. The rack is left as it is.
    pub fn delete(&mut self) -> Result<(), String> {
        if !self.is_user() {
            return Err("Built-in presets cannot be deleted.".into());
        }
        std::fs::remove_file(self.path(&self.current)).map_err(|e| e.to_string())?;
        self.current.clear();
        self.refresh();
        Ok(())
    }

    fn write(&mut self, name: &str, settings: &Settings) -> Result<(), String> {
        let items = settings.items();
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let text = format!("{NAME_KEY}={name}\n{}", rack_to_text(&items));
        std::fs::write(self.path(name), text).map_err(|e| e.to_string())?;
        self.current = name.to_string();
        self.baseline = fingerprint(&items);
        self.refresh();
        Ok(())
    }

    /// The trimmed name, if it may be used for a new file. `own` is the preset being renamed.
    fn check_name(&self, name: &str, own: Option<&str>) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err("Give the preset a name.".into());
        }
        if BUILTINS.iter().any(|b| b.eq_ignore_ascii_case(name)) {
            return Err("That name belongs to a built-in preset.".into());
        }
        let path = self.path(name);
        if path.exists() && own.map(|o| self.path(o)) != Some(path) {
            return Err("A preset with that name already exists.".into());
        }
        Ok(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn fresh() -> Arc<Settings> {
        Settings::new(crate::dsp::default_sections())
    }

    fn param(settings: &Settings, kind: &str, name: &str) -> f32 {
        let item = settings.items().into_iter().find(|i| i.kind() == kind).unwrap();
        let p = item.section.simple.iter().chain(&item.section.params).find(|p| p.name == name);
        p.unwrap_or_else(|| panic!("no {name} in {kind}")).get()
    }

    /// A directory of its own, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("middlemic-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn alexs_preset_has_nine_stages_with_alexs_values() {
        let settings = fresh();
        let mut presets = Presets::new(PathBuf::from("nowhere"), "");
        presets.load(&settings, ALEX_NAME).unwrap();
        assert_eq!(settings.items().len(), 9);
        assert!((param(&settings, "EQ", "High shelf gain") - 3.45).abs() < 1e-3);
        assert_eq!(param(&settings, "Noise removal (DPDFNet)", "Max reduction"), 67.0);
        assert_eq!(param(&settings, "Compressor", "Amount"), 25.0);
        assert!((param(&settings, "Compressor", "Low ratio") - 3.4).abs() < 1e-4);
        assert_eq!(param(&settings, "Compressor", "Master ratio"), 1.0);
        let order: Vec<_> = settings.items().iter().map(|i| i.kind()).collect();
        assert_eq!(order[..3], ["Echo cancellation", "Mic boost", "Noise removal"]);
        assert!(!settings.section("Limiter").is_enabled());
        assert!(settings.section("Limiter").open.load(Relaxed));
    }

    #[test]
    fn default_preset_matches_the_default_rack() {
        let settings = fresh();
        settings.section("EQ").params[0].set(500.0);
        Presets::new(PathBuf::from("nowhere"), "").load(&settings, DEFAULT).unwrap();
        let kinds: Vec<_> = settings.items().iter().map(|i| i.kind()).collect();
        let defaults: Vec<_> = crate::dsp::default_sections().iter().map(|s| s.name).collect();
        assert_eq!(kinds, defaults);
        assert_eq!(fingerprint(&settings.items()), fingerprint(&fresh().items()));
    }

    #[test]
    fn file_names_are_safe_on_windows() {
        assert_eq!(file_name("Podcast"), "Podcast.ini");
        assert_eq!(file_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j.ini");
        assert_eq!(file_name("  .. "), "preset.ini");
        assert_eq!(file_name("con"), "con_.ini");
        assert_eq!(file_name("COM1.x"), "COM1.x_.ini");
        assert_eq!(file_name("Alex's preset!"), "Alex's preset!.ini");
        assert_eq!(file_name("x\ny"), "x_y.ini");
        assert!(file_name(&"a".repeat(300)).len() <= 104);
    }

    #[test]
    fn a_saved_preset_loads_back_into_another_rack() {
        let dir = TempDir::new("round-trip");
        let a = fresh();
        a.section("Compressor").simple[0].set(41.0);
        a.remove(a.items()[0].id);
        let mut presets = Presets::new(dir.0.clone(), "");
        presets.save_as(&a, "Stream: loud?").unwrap();
        assert_eq!(presets.user(), ["Stream: loud?"]);
        assert_eq!(presets.current(), "Stream: loud?");

        let b = fresh();
        let mut later = Presets::new(dir.0.clone(), "Stream: loud?");
        assert_eq!(later.current(), "Stream: loud?", "remembered across launches");
        later.load(&b, "Stream: loud?").unwrap();
        assert_eq!(b.items().len(), 8);
        assert_eq!(param(&b, "Compressor", "Amount"), 41.0);
        assert_eq!(fingerprint(&a.items()), fingerprint(&b.items()));
    }

    #[test]
    fn modified_marker_follows_edits_and_saves() {
        let dir = TempDir::new("modified");
        let settings = fresh();
        let mut presets = Presets::new(dir.0.clone(), "");
        assert_eq!(presets.label(&settings), "Custom");

        presets.load(&settings, ALEX_NAME).unwrap();
        assert_eq!(presets.label(&settings), ALEX_NAME);
        settings.section("EQ").params[0].set(200.0);
        assert!(presets.modified(&settings));
        assert_eq!(presets.label(&settings), format!("{ALEX_NAME} •"));
        assert!(presets.save(&settings).is_err(), "built-ins are not overwritten");

        presets.save_as(&settings, "Mine").unwrap();
        assert!(!presets.modified(&settings));
        settings.section("EQ").params[0].set(300.0);
        assert!(presets.modified(&settings));
        presets.save(&settings).unwrap();
        assert!(!presets.modified(&settings));
    }

    #[test]
    fn a_restored_rack_with_other_ids_is_not_modified() {
        let settings = fresh();
        let mut presets = Presets::new(PathBuf::from("nowhere"), "");
        presets.load(&settings, DEFAULT).unwrap();
        let other = fresh();
        other.remove(other.items()[0].id);
        other.add(crate::dsp::new_section("Echo cancellation").unwrap());
        other.section("Echo cancellation").open.store(settings.section("Echo cancellation").open.load(Relaxed), Relaxed);
        assert!(presets.modified(&other), "the stage moved to the end");
        other.move_item(8, 0);
        assert_ne!(other.items()[0].id, settings.items()[0].id);
        assert!(!presets.modified(&other), "same rack, other ids");
    }

    #[test]
    fn loading_a_preset_bumps_the_rack_generation() {
        let settings = fresh();
        let before = settings.generation();
        Presets::new(PathBuf::from("nowhere"), "").load(&settings, ALEX_NAME).unwrap();
        assert!(settings.generation() > before);
    }

    #[test]
    fn rename_and_delete_keep_the_list_in_step() {
        let dir = TempDir::new("rename");
        let settings = fresh();
        let mut presets = Presets::new(dir.0.clone(), "");
        presets.save_as(&settings, "One").unwrap();
        presets.save_as(&settings, "Tw:o").unwrap();
        assert!(presets.save_as(&settings, "Tw?o").is_err(), "same file");
        assert!(presets.save_as(&settings, "default").is_err(), "built-in name");
        assert!(presets.rename("One").is_err(), "taken");
        presets.rename("Three: 3").unwrap();
        assert_eq!(presets.user(), ["One", "Three: 3"]);
        assert_eq!(presets.current(), "Three: 3");
        assert!(!presets.modified(&settings));

        presets.delete().unwrap();
        assert_eq!(presets.user(), ["One"]);
        assert_eq!(presets.current(), "");
        presets.load(&settings, ALEX_NAME).unwrap();
        assert!(presets.delete().is_err());
    }
}
