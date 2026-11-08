//! Launch when Windows starts: a value under HKCU\...\Run, so no administrator is needed.
//! Uses the `reg` tool that ships with Windows, so there is no registry code to get wrong.

const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "middlemic";

/// The command Windows runs at sign-in: quoted path, starting minimised.
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

#[cfg(windows)]
pub use runner::{is_enabled, set_enabled};

#[cfg(windows)]
mod runner {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<bool> {
        Ok(Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output()?.status.success())
    }

    pub fn is_enabled() -> bool {
        reg(&["query", KEY, "/v", VALUE]).unwrap_or(false)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let done = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            reg(&["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &command_line(&exe), "/f"])
        } else {
            reg(&["delete", KEY, "/v", VALUE, "/f"])
        };
        match done {
            Ok(true) => Ok(()),
            Ok(false) => Err("Windows refused the change to the startup list.".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_quotes_the_path_and_starts_minimised() {
        let line = command_line(std::path::Path::new(r"C:\Program Files\middlemic\middlemic.exe"));
        assert_eq!(line, r#""C:\Program Files\middlemic\middlemic.exe" --minimized"#);
    }
}
