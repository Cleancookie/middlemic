#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod audio;
mod dsp;
mod eq_panel;
mod knob;
mod rack;
mod settings;
mod setup;
mod spectrum;
#[cfg(windows)]
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Lightweight mic processing: headset in, cleaned audio out to a virtual cable")]
struct Cli {
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

#[cfg(windows)]
fn main() -> Result<()> {
    use std::sync::Arc;
    use std::time::Duration;

    let settings = settings::Settings::new(dsp::default_sections());
    match Cli::parse().command {
        None => {
            // SAFETY: detaches the console a double-click launch created; no pointers involved.
            unsafe { windows_sys::Win32::System::Console::FreeConsole() };
            ui::run(settings).map_err(|e| anyhow::anyhow!("{e}"))
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
    let _ = Cli::parse();
    anyhow::bail!("middlemic only runs on Windows; build with --target x86_64-pc-windows-gnu")
}
