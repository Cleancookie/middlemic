//! What middlemic keeps in `%APPDATA%\middlemic`, so the settings page can show it and delete it.

use std::path::{Path, PathBuf};

pub struct Entry {
    pub label: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
}

/// The things worth listing on their own, as paths under the data directory.
const KNOWN: [(&str, &str); 3] = [("AI models", "models"), ("Presets", "presets"), ("Settings", "settings.ini")];

pub fn data_dir() -> PathBuf {
    crate::settings::settings_path().parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn size(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| size(&e.path())).sum(),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// The known entries that exist under `dir`.
pub fn list(dir: &Path) -> Vec<Entry> {
    KNOWN
        .iter()
        .map(|(label, name)| Entry { label, path: dir.join(name), bytes: 0 })
        .filter(|e| e.path.exists())
        .map(|e| Entry { bytes: size(&e.path), ..e })
        .collect()
}

pub fn delete(path: &Path) -> std::io::Result<()> {
    if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }
}

/// Delete everything middlemic keeps, the data directory included.
pub fn delete_all(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("middlemic-storage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models").join("a.onnx"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("settings.ini"), "x=1\n").unwrap();
        dir
    }

    #[test]
    fn lists_what_exists_with_sizes_and_deletes_each_on_its_own() {
        let dir = temp("list");
        let entries = list(&dir);
        let seen: Vec<_> = entries.iter().map(|e| (e.label, e.bytes)).collect();
        assert_eq!(seen, [("AI models", 3000), ("Settings", 4)]);

        delete(&entries[0].path).unwrap();
        assert!(!dir.join("models").exists() && dir.join("settings.ini").exists());
        assert_eq!(list(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_all_removes_the_directory_and_is_fine_when_it_is_gone() {
        let dir = temp("all");
        delete_all(&dir).unwrap();
        assert!(!dir.exists());
        delete_all(&dir).unwrap();
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(10 * 1024 * 1024), "10.0 MB");
    }
}
