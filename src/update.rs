//! Finding, downloading and installing new releases from GitHub. Downloading and installing
//! are separate steps, so a download can wait until the user is ready to restart.
//!
//! Only builds made by the release workflow know their version (`MIDDLEMIC_VERSION`); a local
//! build is "dev" and never updates. Network work goes through the PowerShell that ships with
//! Windows, like `setup.rs`, so there is no HTTP dependency.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const REPO: &str = "Cleancookie/middlemic";
pub const EXE_ASSET: &str = "middlemic.exe";
pub const SUMS_ASSET: &str = "SHA256SUMS";

pub fn current_version() -> &'static str {
    option_env!("MIDDLEMIC_VERSION").unwrap_or("dev")
}

/// `v1.2.3` as numbers. Anything else, such as `dev`, is not a release.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.strip_prefix('v')?.split('.').map(|p| p.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

pub fn is_release_build(version: &str) -> bool {
    parse_version(version).is_some()
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub exe_url: String,
    pub sums_url: String,
}

/// Reads the `tag=`, `exe=` and `sums=` lines the check script prints.
pub fn parse_release(output: &str) -> Option<Release> {
    let field = |key: &str| {
        output.lines().find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    Some(Release { tag: field("tag=")?, exe_url: field("exe=")?, sums_url: field("sums=")? })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing checked yet, or this is a dev build.
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Downloaded and checked against SHA256SUMS; waiting for the user to install.
    Ready(Release, PathBuf),
}

struct Inner {
    status: Status,
    /// Where to go back to if a check fails.
    before_check: Status,
    error: String,
}

#[derive(Clone)]
pub struct Updater {
    inner: Arc<Mutex<Inner>>,
    dir: PathBuf,
    current: String,
}

impl Updater {
    /// `dir` holds downloaded updates; `current` is the running version.
    pub fn new(dir: PathBuf, current: &str) -> Self {
        let inner = Inner { status: Status::Idle, before_check: Status::Idle, error: String::new() };
        Self { inner: Arc::new(Mutex::new(inner)), dir, current: current.to_string() }
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    pub fn status(&self) -> Status {
        self.inner.lock().unwrap().status.clone()
    }

    /// Why the last check or download failed; empty when it did not.
    pub fn error(&self) -> String {
        self.inner.lock().unwrap().error.clone()
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.status(), Status::Checking | Status::Downloading(_))
    }

    pub fn staged_path(&self, tag: &str) -> PathBuf {
        self.dir.join(format!("middlemic-{tag}.exe"))
    }

    /// Moves to `Checking` unless this is a dev build or something is already running.
    fn start_check(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if !is_release_build(&self.current) || matches!(inner.status, Status::Checking | Status::Downloading(_)) {
            return false;
        }
        inner.before_check = std::mem::replace(&mut inner.status, Status::Checking);
        inner.error.clear();
        true
    }

    fn finish_check(&self, result: Result<Release, String>) {
        let status = match result {
            Err(why) => {
                let mut inner = self.inner.lock().unwrap();
                inner.status = std::mem::replace(&mut inner.before_check, Status::Idle);
                inner.error = why;
                return;
            }
            Ok(release) if !is_newer(&self.current, &release.tag) => {
                self.prune(None);
                Status::UpToDate
            }
            Ok(release) => {
                self.prune(Some(&release.tag));
                let staged = self.staged_path(&release.tag);
                if staged.is_file() { Status::Ready(release, staged) } else { Status::Available(release) }
            }
        };
        self.inner.lock().unwrap().status = status;
    }

    /// Deletes downloads (and half-downloads) for any version but `keep`.
    fn prune(&self, keep: Option<&str>) {
        let keep = keep.map(|tag| format!("middlemic-{tag}.exe"));
        for entry in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("middlemic-") && Some(&name) != keep.as_ref() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Moves `Available` to `Downloading` and returns what to fetch.
    fn start_download(&self) -> Option<Release> {
        let mut inner = self.inner.lock().unwrap();
        let Status::Available(release) = inner.status.clone() else { return None };
        inner.status = Status::Downloading(release.clone());
        inner.error.clear();
        Some(release)
    }

    fn finish_download(&self, release: Release, result: Result<PathBuf, String>) {
        let mut inner = self.inner.lock().unwrap();
        match result {
            Ok(path) => inner.status = Status::Ready(release, path),
            Err(why) => {
                inner.status = Status::Available(release);
                inner.error = why;
            }
        }
    }

    fn ready(&self) -> Option<PathBuf> {
        match self.status() {
            Status::Ready(_, path) => Some(path),
            _ => None,
        }
    }
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Puts `staged` in place of the running `exe`. Windows will not overwrite a running exe but
/// does allow renaming it, so the old one moves aside to `.old` first. Undone if the copy fails.
pub fn swap_in(exe: &Path, staged: &Path) -> std::io::Result<()> {
    if !staged.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the downloaded update is missing"));
    }
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(err) = std::fs::copy(staged, exe) {
        let _ = std::fs::remove_file(exe);
        let _ = std::fs::rename(&old, exe);
        return Err(err);
    }
    let _ = std::fs::remove_file(staged);
    Ok(())
}

/// Deletes the previous exe left behind by an install. Skipped quietly while it is still running.
pub fn remove_previous_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

/// Prints `tag=`, `exe=` and `sums=` for the latest release.
fn check_script() -> String {
    let api = crate::setup::ps_quote(&format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let (exe, sums) = (crate::setup::ps_quote(EXE_ASSET), crate::setup::ps_quote(SUMS_ASSET));
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         $r=Invoke-RestMethod -Uri {api} -Headers @{{'User-Agent'='middlemic-updater'}}; \
         Write-Output ('tag=' + $r.tag_name); \
         foreach($a in $r.assets){{ \
         if($a.name -eq {exe}){{ Write-Output ('exe=' + $a.browser_download_url) }}; \
         if($a.name -eq {sums}){{ Write-Output ('sums=' + $a.browser_download_url) }} }}"
    )
}

/// Downloads the exe through a `.part` file and only moves it to `final_path` if its SHA-256
/// matches the line for middlemic.exe in the release's SHA256SUMS. Prints the final path.
fn download_script(release: &Release, dir: &Path, final_path: &Path) -> String {
    use crate::setup::ps_quote;
    let (exe_url, sums_url) = (ps_quote(&release.exe_url), ps_quote(&release.sums_url));
    let (dir, final_path) = (ps_quote(&dir.to_string_lossy()), ps_quote(&final_path.to_string_lossy()));
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         New-Item -ItemType Directory -Force -Path {dir} | Out-Null; \
         $final={final_path}; $part=$final + '.part'; $sums=$final + '.sums.part'; \
         Invoke-WebRequest -Uri {sums_url} -OutFile $sums -UseBasicParsing; \
         $want=$null; foreach($l in (Get-Content -LiteralPath $sums)){{ \
         if($l -match '^([0-9a-fA-F]{{64}})\\s+\\*?middlemic\\.exe\\s*$'){{ $want=$Matches[1].ToLower() }} }}; \
         Remove-Item -LiteralPath $sums -Force; \
         if(-not $want){{ throw 'SHA256SUMS has no entry for middlemic.exe' }}; \
         Invoke-WebRequest -Uri {exe_url} -OutFile $part -UseBasicParsing; \
         $hash=(Get-FileHash -Algorithm SHA256 -LiteralPath $part).Hash.ToLower(); \
         if($hash -ne $want){{ Remove-Item -LiteralPath $part -Force; \
         throw ('the download does not match its checksum (SHA-256 ' + $hash + ')') }}; \
         Move-Item -LiteralPath $part -Destination $final -Force; Write-Output $final"
    )
}

#[cfg(windows)]
impl Updater {
    /// Looks for a newer release in the background. Does nothing on a dev build.
    pub fn check(&self) {
        if !self.start_check() {
            return;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let result = crate::setup::powershell(&check_script())
                .map_err(|e| format!("could not check for updates: {e}"))
                .and_then(|(_, out)| parse_release(&out).ok_or_else(|| "the latest release has no middlemic.exe".to_string()));
            this.finish_check(result);
        });
    }

    /// Fetches the available release in the background and checks it. Does not install it.
    pub fn download(&self) {
        let Some(release) = self.start_download() else { return };
        let this = self.clone();
        std::thread::spawn(move || {
            let target = this.staged_path(&release.tag);
            let result = crate::setup::powershell(&download_script(&release, &this.dir, &target))
                .map(|_| target)
                .map_err(|e| format!("download failed: {e}"));
            this.finish_download(release, result);
        });
    }

    /// Replaces the running exe with the downloaded one and starts it with the same arguments.
    /// The caller then exits.
    pub fn install(&self) -> Result<(), String> {
        let staged = self.ready().ok_or("no update has been downloaded")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        swap_in(&exe, &staged).map_err(|e| format!("could not replace {}: {e}", exe.display()))?;
        restart(&exe).map_err(|e| format!("updated, but could not restart: {e}. Start middlemic again."))
    }
}

/// Starts `exe` again with this run's arguments. The console was freed at startup, so the standard
/// handles are invalid; handing those to the child fails with "request is not supported" (os error 50).
fn restart(exe: &Path) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command.args(std::env::args_os().skip(1)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("middlemic-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn release(tag: &str) -> Release {
        Release { tag: tag.into(), exe_url: format!("https://x/{tag}/middlemic.exe"), sums_url: format!("https://x/{tag}/SHA256SUMS") }
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("dev"), None);
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert!(is_newer("v0.9.0", "v0.10.0"));
        assert!(!is_newer("v0.10.0", "v0.10.0"));
        assert!(!is_newer("dev", "v9.9.9"), "a dev build never updates");
    }

    #[test]
    fn the_check_output_becomes_a_release() {
        let out = "tag=v0.2.0\r\nexe=https://x/middlemic.exe\r\nsums=https://x/SHA256SUMS\r\n";
        assert_eq!(parse_release(out).unwrap().tag, "v0.2.0");
        assert!(parse_release("tag=v0.2.0\nexe=https://x/middlemic.exe\n").is_none(), "no checksums, no update");
    }

    #[test]
    fn a_newer_release_is_available_and_downloading_is_a_separate_step() {
        let dir = temp("flow");
        let up = Updater::new(dir.clone(), "v0.1.0");
        assert!(up.start_check());
        assert_eq!(up.status(), Status::Checking);
        assert!(!up.start_check(), "one check at a time");
        up.finish_check(Ok(release("v0.2.0")));
        assert_eq!(up.status(), Status::Available(release("v0.2.0")));

        let wanted = up.start_download().unwrap();
        assert!(matches!(up.status(), Status::Downloading(_)));
        assert!(up.start_download().is_none());
        let path = up.staged_path(&wanted.tag);
        up.finish_download(wanted, Ok(path.clone()));
        assert_eq!(up.status(), Status::Ready(release("v0.2.0"), path.clone()));
        assert_eq!(up.ready(), Some(path));
    }

    #[test]
    fn a_failed_download_can_be_tried_again() {
        let up = Updater::new(temp("fail"), "v0.1.0");
        up.start_check();
        up.finish_check(Ok(release("v0.2.0")));
        let wanted = up.start_download().unwrap();
        up.finish_download(wanted, Err("offline".into()));
        assert_eq!(up.status(), Status::Available(release("v0.2.0")));
        assert_eq!(up.error(), "offline");
        assert!(up.start_download().is_some());
    }

    #[test]
    fn a_failed_check_keeps_what_was_known() {
        let up = Updater::new(temp("check-fail"), "v0.1.0");
        up.start_check();
        up.finish_check(Ok(release("v0.2.0")));
        up.start_check();
        up.finish_check(Err("offline".into()));
        assert_eq!(up.status(), Status::Available(release("v0.2.0")));
        assert_eq!(up.error(), "offline");
    }

    #[test]
    fn a_download_from_an_earlier_run_is_ready_without_downloading_again() {
        let dir = temp("staged");
        std::fs::write(dir.join("middlemic-v0.2.0.exe"), b"exe").unwrap();
        std::fs::write(dir.join("middlemic-v0.1.5.exe"), b"old").unwrap();
        let up = Updater::new(dir.clone(), "v0.1.0");
        up.start_check();
        up.finish_check(Ok(release("v0.2.0")));
        assert!(matches!(up.status(), Status::Ready(..)));
        assert!(!dir.join("middlemic-v0.1.5.exe").exists(), "stale downloads are cleared");
    }

    #[test]
    fn up_to_date_and_dev_builds_do_not_offer_anything() {
        let up = Updater::new(temp("current"), "v0.2.0");
        up.start_check();
        up.finish_check(Ok(release("v0.2.0")));
        assert_eq!(up.status(), Status::UpToDate);
        assert!(!Updater::new(temp("dev"), "dev").start_check());
    }

    #[test]
    fn installing_moves_the_old_exe_aside_and_keeps_it_on_failure() {
        let dir = temp("swap");
        let (exe, staged) = (dir.join("middlemic.exe"), dir.join("new.exe"));
        std::fs::write(&exe, b"old").unwrap();
        assert!(swap_in(&exe, &dir.join("missing.exe")).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old", "untouched when there is nothing to install");

        std::fs::write(&staged, b"new").unwrap();
        swap_in(&exe, &staged).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!staged.exists());
    }

    #[test]
    fn scripts_never_contain_double_quotes_and_name_what_they_fetch() {
        let check = check_script();
        assert!(!check.contains('"') && check.contains("Cleancookie/middlemic/releases/latest"));
        let dir = PathBuf::from("C:\\Users\\a b\\updates");
        let download = download_script(&release("v0.2.0"), &dir, &dir.join("middlemic-v0.2.0.exe"));
        assert!(!download.contains('"'));
        assert!(download.contains("https://x/v0.2.0/SHA256SUMS") && download.contains("middlemic\\.exe"));
        assert!(download.contains("Move-Item") && download.find("-ne $want").unwrap() < download.find("Move-Item").unwrap());
    }
}
