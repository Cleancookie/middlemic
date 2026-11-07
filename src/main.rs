#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod audio;
mod dsp;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Lightweight mic processing: headset in, cleaned audio out to a virtual cable")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List recording and playback devices.
    Devices,
    /// Capture the mic, run the chain, play to the output device.
    Run {
        /// Part of the mic's name. Default: the system default recording device.
        #[arg(long)]
        input: Option<String>,
        /// Part of the output device's name, e.g. "CABLE Input".
        #[arg(long)]
        output: Option<String>,
        /// Mic boost in dB.
        #[arg(long, default_value_t = 0.0)]
        gain_db: f32,
        /// Skip DeepFilterNet noise removal.
        #[arg(long)]
        no_denoise: bool,
        /// Cap noise removal at this many dB (25-40 sounds more natural). Default: no cap.
        #[arg(long)]
        denoise_limit_db: Option<f32>,
    },
}

#[cfg(windows)]
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Devices => audio::list_devices(),
        Command::Run {
            input,
            output,
            gain_db,
            no_denoise,
            denoise_limit_db,
        } => {
            audio::run(input, output, move || {
                let mut chain = dsp::Chain::new();
                if !no_denoise {
                    chain.push(dsp::DeepFilter::new(denoise_limit_db)?);
                }
                chain.push(dsp::Gain::new(gain_db));
                chain.push(dsp::Limiter::new(-1.0));
                Ok(chain)
            })
        }
    }
}

#[cfg(not(windows))]
fn main() -> Result<()> {
    let _ = Cli::parse();
    anyhow::bail!("middlemic only runs on Windows; build with --target x86_64-pc-windows-gnu")
}
