//! One-time setup: download VB-CABLE from VB-Audio, check the installer is signed, and run
//! it elevated. Uses the PowerShell that ships with Windows, so no HTTP or zip dependencies.
//!
//! VB-CABLE is donationware by VB-Audio (www.vb-cable.com). Their licensing page allows
//! installing it alongside another app as long as users can see where it came from.

pub const DOWNLOAD_URL: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip";
pub const INSTALLER_NAME: &str = "VBCABLE_Setup_x64.exe";
/// Playback side of the cable. Programs choose its recording side, "CABLE Output".
pub const CABLE_OUTPUT_NAME: &str = "CABLE Input";

/// `playback_devices` are friendly names, as listed by [`crate::audio::list_devices`].
pub fn cable_present(playback_devices: &[String]) -> bool {
    playback_devices.iter().any(|n| n.contains(CABLE_OUTPUT_NAME))
}

/// Quote for a PowerShell single-quoted string. Avoids double quotes entirely, which
/// Windows argument passing mangles.
pub(crate) fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Downloads, extracts and signature-checks. Prints the installer's path, then the signer.
fn download_script(work_dir: &str) -> String {
    let dir = ps_quote(work_dir);
    let url = ps_quote(DOWNLOAD_URL);
    let name = ps_quote(INSTALLER_NAME);
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         $dir={dir}; New-Item -ItemType Directory -Force -Path $dir | Out-Null; \
         $zip=Join-Path $dir 'pack.zip'; \
         Invoke-WebRequest -Uri {url} -OutFile $zip -UseBasicParsing; \
         $out=Join-Path $dir 'pack'; Expand-Archive -LiteralPath $zip -DestinationPath $out -Force; \
         $exe=Get-ChildItem -Path $out -Recurse -Filter {name} | Select-Object -First 1; \
         if(-not $exe){{ throw ('' + {name} + ' was not in the download') }}; \
         $sig=Get-AuthenticodeSignature -FilePath $exe.FullName; \
         $who=''; if($sig.SignerCertificate){{ $who=$sig.SignerCertificate.Subject }}; \
         if($sig.Status -ne 'Valid'){{ throw ('installer signature is ' + $sig.Status + ' ' + $who) }}; \
         if($who -notmatch 'VB-Audio|BUREL VINCENT'){{ throw ('installer is signed by someone unexpected: ' + $who) }}; \
         Write-Output $exe.FullName; Write-Output $who"
    )
}

/// A file the app downloads on demand, checked against a pinned SHA-256 before it is used.
#[derive(Clone, Copy)]
pub struct Fetch {
    pub url: &'static str,
    /// Where it lands in the download folder.
    pub file: &'static str,
    /// Of the downloaded file: the whole archive when `member` is set.
    pub sha256: &'static str,
    /// The one file to take out of a zip download, by its path inside the archive.
    pub member: Option<&'static str>,
    pub megabytes: u32,
}

/// Downloads `url` into `dir/file` through a `.part` file, and only moves it into place if its
/// SHA-256 is `sha256`. With `member`, the download is a zip and only that entry is kept.
/// Prints the final path.
fn model_download_script(url: &str, dir: &str, file: &str, sha256: &str, member: Option<&str>) -> String {
    let (url, dir, file, sha) = (ps_quote(url), ps_quote(dir), ps_quote(file), ps_quote(&sha256.to_lowercase()));
    let place = match member {
        None => "Move-Item -LiteralPath $part -Destination $final -Force; ".to_string(),
        Some(member) => format!(
            "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
             $tmp=$final + '.tmp'; $zip=[IO.Compression.ZipFile]::OpenRead($part); \
             try{{ $entry=$zip.GetEntry({}); if(-not $entry){{ throw 'the archive is missing a file' }}; \
             [IO.Compression.ZipFileExtensions]::ExtractToFile($entry,$tmp,$true) }} finally {{ $zip.Dispose() }}; \
             Remove-Item -LiteralPath $part -Force; Move-Item -LiteralPath $tmp -Destination $final -Force; ",
            ps_quote(member)
        ),
    };
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         $dir={dir}; New-Item -ItemType Directory -Force -Path $dir | Out-Null; \
         $final=Join-Path $dir {file}; $part=$final + '.part'; \
         Invoke-WebRequest -Uri {url} -OutFile $part -UseBasicParsing; \
         $hash=(Get-FileHash -Algorithm SHA256 -LiteralPath $part).Hash.ToLower(); \
         if($hash -ne {sha}){{ Remove-Item -LiteralPath $part -Force; \
         throw ('the download is not the expected file (SHA-256 ' + $hash + ')') }}; \
         {place}Write-Output $final"
    )
}

/// Runs the installer silently with an administrator (UAC) prompt, and waits for it.
fn install_script(installer: &str) -> String {
    let exe = ps_quote(installer);
    format!(
        "$ErrorActionPreference='Stop'; \
         $p=Start-Process -FilePath {exe} -ArgumentList '-i','-h' \
         -WorkingDirectory (Split-Path {exe}) -Verb RunAs -Wait -PassThru; \
         exit $p.ExitCode"
    )
}

#[cfg(windows)]
pub use runner::{Progress, powershell, spawn_downloads, spawn_install};

#[cfg(windows)]
mod runner {
    use super::*;
    use anyhow::{Result, anyhow, bail};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DEVICE_WAIT: Duration = Duration::from_secs(30);

    #[derive(Default)]
    pub struct Progress {
        pub message: String,
        /// `None` while running.
        pub outcome: Option<Result<String, String>>,
    }

    pub fn spawn_install() -> Arc<Mutex<Progress>> {
        spawn_task(run)
    }

    /// Download each of `files` into `dir`, one after the other. The outcome lists what was saved.
    pub fn spawn_downloads(files: Vec<Fetch>, dir: PathBuf) -> Arc<Mutex<Progress>> {
        spawn_task(move |say| {
            let mut saved = Vec::new();
            for f in files {
                say(&format!("Downloading {} ({} MB)…", f.file, f.megabytes));
                let script = model_download_script(f.url, &dir.to_string_lossy(), f.file, f.sha256, f.member);
                let (_, path) = powershell(&script).map_err(|e| anyhow!("download of {} failed: {e}", f.file))?;
                saved.push(path);
            }
            Ok(format!("Saved {}", saved.join(", ")))
        })
    }

    /// Run `task` on its own thread, reporting through the returned progress.
    fn spawn_task(task: impl FnOnce(&dyn Fn(&str)) -> Result<String> + Send + 'static) -> Arc<Mutex<Progress>> {
        let progress = Arc::new(Mutex::new(Progress::default()));
        let shared = progress.clone();
        thread::spawn(move || {
            let say = |m: &str| shared.lock().unwrap().message = m.to_string();
            let outcome = task(&say).map_err(|e| format!("{e:#}"));
            shared.lock().unwrap().outcome = Some(outcome);
        });
        progress
    }

    pub fn powershell(script: &str) -> Result<(i32, String)> {
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        let code = out.status.code().unwrap_or(-1);
        if code != 0 && !out.stderr.is_empty() {
            let reason = String::from_utf8_lossy(&out.stderr);
            // PowerShell prefixes errors with location noise; the first line is the message.
            bail!("{}", reason.lines().next().unwrap_or("").trim());
        }
        Ok((code, String::from_utf8_lossy(&out.stdout).trim().to_string()))
    }

    fn run(say: &dyn Fn(&str)) -> Result<String> {
        let work_dir = std::env::temp_dir().join(format!("middlemic-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work_dir);

        say("Downloading VB-CABLE from vb-audio.com…");
        let (_, out) = powershell(&download_script(&work_dir.to_string_lossy()))
            .map_err(|e| anyhow!("download or check failed: {e}"))?;
        let mut lines = out.lines();
        let installer = lines.next().ok_or_else(|| anyhow!("no installer path returned"))?.trim().to_string();
        let signer = lines.next().unwrap_or("").trim().to_string();

        say(&format!(
            "Signature OK ({signer}). Installing: accept the Windows admin prompt, then the driver prompt…"
        ));
        let (exit_code, _) = powershell(&install_script(&installer)).map_err(|e| {
            if e.to_string().contains("canceled") || e.to_string().contains("cancelled") {
                anyhow!("the admin prompt was declined")
            } else {
                anyhow!("install failed: {e}")
            }
        })?;

        say("Waiting for the CABLE device to appear…");
        let started = std::time::Instant::now();
        let found = loop {
            if crate::audio::list_devices().is_ok_and(|d| cable_present(&d.outputs)) {
                break true;
            }
            if started.elapsed() > DEVICE_WAIT {
                break false;
            }
            thread::sleep(Duration::from_secs(1));
        };
        let _ = std::fs::remove_dir_all(&work_dir);

        if found {
            Ok(format!("VB-CABLE installed (installer exit code {exit_code}). \"{CABLE_OUTPUT_NAME}\" is ready."))
        } else {
            bail!(
                "the installer finished (exit code {exit_code}) but \"{CABLE_OUTPUT_NAME}\" has not appeared. \
                 VB-Audio recommends a reboot after installing."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_doubles_apostrophes() {
        assert_eq!(ps_quote(r"C:\Users\O'Brien\Temp"), r"'C:\Users\O''Brien\Temp'");
    }

    #[test]
    fn scripts_never_contain_double_quotes() {
        assert!(!download_script(r"C:\Temp\x y").contains('"'));
        assert!(!install_script(r"C:\Temp\VBCABLE_Setup_x64.exe").contains('"'));
    }

    #[test]
    fn download_script_checks_signature_before_returning_the_path() {
        let s = download_script(r"C:\Temp\x");
        let check = s.find("Get-AuthenticodeSignature").unwrap();
        let print = s.find("Write-Output $exe.FullName").unwrap();
        assert!(check < print);
        assert!(s.contains(DOWNLOAD_URL));
        // The real installer is signed by its author's name, not by "VB-Audio".
        assert!(s.contains("BUREL VINCENT"));
    }

    /// Manual probe of the real download without installing anything:
    /// `MIDDLEMIC_PROBE_OUT=script.ps1 MIDDLEMIC_PROBE_DIR='C:\x' cargo test write_probe -- --ignored`
    #[test]
    #[ignore]
    fn write_probe_script() {
        let out = std::env::var("MIDDLEMIC_PROBE_OUT").unwrap();
        let dir = std::env::var("MIDDLEMIC_PROBE_DIR").unwrap();
        std::fs::write(out, download_script(&dir)).unwrap();
    }

    #[test]
    fn model_download_checks_the_hash_before_moving_the_file_into_place() {
        let s = model_download_script("https://example.test/m.onnx", r"C:\Users\O'Brien\m", "m.onnx", "ABC123", None);
        assert!(!s.contains('"'));
        assert!(s.contains("https://example.test/m.onnx") && s.contains("'abc123'"), "{s}");
        assert!(s.contains(r"'C:\Users\O''Brien\m'"));
        let check = s.find("-ne").unwrap();
        let place = s.find("Move-Item").unwrap();
        let remove = s.find("Remove-Item").unwrap();
        assert!(check < remove && remove < place, "a wrong file is deleted, never moved into place");
    }

    /// Writes the real model download script to run under PowerShell:
    /// `MIDDLEMIC_PROBE_OUT=script.ps1 MIDDLEMIC_PROBE_DIR=/out MIDDLEMIC_PROBE_SHA=<hash> cargo test write_model_probe -- --ignored`
    #[test]
    #[ignore]
    fn write_model_probe() {
        let model = crate::dsp::DPDFNET;
        let out = std::env::var("MIDDLEMIC_PROBE_OUT").unwrap();
        let dir = std::env::var("MIDDLEMIC_PROBE_DIR").unwrap();
        let sha = std::env::var("MIDDLEMIC_PROBE_SHA").unwrap_or_else(|_| model.sha256.to_string());
        std::fs::write(out, model_download_script(model.url, &dir, model.file, &sha, None)).unwrap();
    }

    #[test]
    fn runtime_download_keeps_one_entry_of_the_checked_zip() {
        let s = model_download_script("https://example.test/r.zip", r"C:\m", "onnxruntime.dll", "ABC", Some("pkg/lib/onnxruntime.dll"));
        assert!(!s.contains('"'));
        let (check, open, place) = (s.find("-ne").unwrap(), s.find("OpenRead").unwrap(), s.rfind("Move-Item").unwrap());
        assert!(check < open && open < place, "the zip is only opened after its hash matched");
        assert!(s.contains("GetEntry('pkg/lib/onnxruntime.dll')"), "{s}");
    }

    #[test]
    fn install_runs_elevated_and_silent() {
        let s = install_script(r"C:\Temp\VBCABLE_Setup_x64.exe");
        assert!(s.contains("-Verb RunAs") && s.contains("'-i','-h'"));
    }

    #[test]
    fn detects_the_cable_by_name() {
        let with = vec!["CABLE Input (VB-Audio Virtual Cable)".to_string()];
        let without = vec!["Speakers".to_string()];
        assert!(cable_present(&with) && !cable_present(&without));
    }
}
