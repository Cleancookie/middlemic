#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod audio;
mod compressor_panel;
#[cfg(windows)]
mod cpu;
mod dsp;
mod eq_panel;
mod feedback_panel;
mod icons;
mod knob;
mod latency;
mod level;
mod live_spectrum;
mod motion;
#[cfg(test)]
mod offline;
mod pcm;
mod playout;
mod presets;
mod profile;
mod rack;
mod recordings_panel;
mod render;
mod settings;
mod settings_page;
mod setup;
mod spectrum;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod tape;
mod storage;
mod theme;
#[cfg(windows)]
mod tray;
mod update;
#[cfg(windows)]
mod ui;
mod wav;
mod wizard;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Lightweight mic processing: headset in, cleaned audio out to a virtual cable")]
struct Cli {
    /// Open the window minimised. Used by "Launch when Windows starts".
    #[arg(long)]
    minimized: bool,
    /// Headless profiler: run CLIP (or synthetic noisy speech) through the live chain and print per-stage timings.
    #[arg(long, value_name = "CLIP.WAV", num_args = 0..=1)]
    profile: Option<Option<std::path::PathBuf>>,
    /// With --profile: hops to measure after 100 warm-up hops.
    #[arg(long, default_value_t = 1000)]
    hops: usize,
    /// With --profile: print JSON instead of a table.
    #[arg(long)]
    json: bool,
    /// With --profile: repeat forever, so an external profiler can attach.
    #[arg(long = "loop")]
    repeat: bool,
    /// With no command, opens the window.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List recording and playback devices.
    Devices,
    /// Run without a window, using the settings saved by the UI.
    Run {
        /// Part of the mic's name. Default: the system default recording device.
        #[arg(long)]
        input: Option<String>,
        /// Part of the output device's name, e.g. "CABLE Input".
        #[arg(long)]
        output: String,
    },
}

impl Cli {
    fn profile_args(&self) -> Option<profile::Args> {
        let clip = self.profile.clone()?;
        Some(profile::Args { clip, hops: self.hops, json: self.json, repeat: self.repeat })
    }
}

#[cfg(windows)]
fn main() -> Result<()> {
    use std::sync::Arc;
    use std::time::Duration;

    let settings = settings::Settings::new(dsp::default_sections());
    let cli = Cli::parse();
    // The exe is a console-subsystem binary, so --profile prints to the launching console as `devices` does.
    if let Some(args) = cli.profile_args() {
        return profile::main(args);
    }
    match cli.command {
        None => {
            // SAFETY: detaches the console a double-click launch created; no pointers involved.
            unsafe { windows_sys::Win32::System::Console::FreeConsole() };
            ui::run(settings, cli.minimized).map_err(|e| anyhow::anyhow!("{e}"))
        }
        Some(Command::Devices) => {
            let devices = audio::list_devices()?;
            println!("Recording devices:");
            devices.inputs.iter().for_each(|d| println!("  {d}"));
            println!("Playback devices:");
            devices.outputs.iter().for_each(|d| println!("  {d}"));
            Ok(())
        }
        Some(Command::Run { input, output }) => {
            settings.apply_text(&std::fs::read_to_string(settings::settings_path()).unwrap_or_default());
            let engine = audio::Engine::start(input, output, Arc::clone(&settings))?;
            eprintln!("running; press Ctrl-C to stop");
            while engine.is_running() {
                std::thread::sleep(Duration::from_millis(500));
            }
            anyhow::bail!(engine.error().unwrap_or_else(|| "stopped".into()))
        }
    }
}

#[cfg(not(windows))]
fn main() -> Result<()> {
    if let Some(args) = Cli::parse().profile_args() {
        return profile::main(args);
    }
    anyhow::bail!("middlemic only runs on Windows; build with --target x86_64-pc-windows-gnu")
}
