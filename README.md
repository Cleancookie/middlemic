# middlemic

Lightweight mic processing for Windows. Takes your headset mic, runs it through a
chain of effects (noise removal, EQ, de-esser, compression, limiter), and plays the
result into a virtual audio cable so Discord, games and OBS can use it as a normal
recording device.

```
Headset mic ──► middlemic (chain) ──► virtual cable ──► "CABLE Output" recording device
```

No DAW, no Virtual Audio Cable routing maze, no heavy plugin host. One small
executable.

## Status

Early. Working today:

- Capture from any recording device, process, play to any playback device (WASAPI, shared mode)
- Pluggable chain: every stage implements `Processor`, so stages can be swapped, reordered or stacked
- Stages, in order: echo cancellation (off by default), DeepFilterNet3 noise removal, mic boost,
  EQ, de-esser, 3-band compressor, limiter
- Three noise removers to compare, switch one on at a time: DeepFilterNet3 (the default),
  DPDFNet and RNNoise (both off by default, each with its own Max reduction). DPDFNet needs
  `dpdfnet2_48khz_hr.onnx` (Ceva, Apache-2.0, from huggingface.co/Ceva-IP/DPDFNet): the row
  has a Download model button that fetches a pinned revision, checks its SHA-256 and saves it
  to `%APPDATA%\middlemic\models`. A copy in a `models` folder next to the exe works too. On a quiet test recording DPDFNet lost less speech than DeepFilterNet at the same
  cap but costs about 10 times the CPU (40% of a core) and adds 50 ms; RNNoise costs about 1%
- Echo cancellation (WebRTC AEC3, pure Rust) works without noise removal. It listens to what
  your speakers play through WASAPI loopback of the default playback device, so set Windows
  playback to your speakers or headphones, not to the virtual cable. If that is the output
  device, the stage stays idle rather than erase your own voice
- Simple window: device pickers, start/stop, level meters, and the stages as a rack of rows
  (fold arrow, bypass switch, reset, live gain-reduction readout); fold state is remembered.
  Drag a row's grip to reorder the chain while it runs; the order is saved too
- EQ screen in the style of a plugin EQ: response curve over a live spectrum, draggable band
  dots, a row of knobs per band, and a scrolling spectrogram with the de-esser frequency marked
- Gain-reduction meters on the de-esser and compressor; the compressor has a Simple mode
  (one Amount slider plus Gentle, Voice and Strong) and an Advanced mode with every dial
- Settings and device choices are saved to `%APPDATA%\middlemic\settings.ini`
- Whole chain costs about 4% of one core in testing (DeepFilterNet about 1.6% of that),
  and the DSP thread runs at below-normal priority

Planned:

- [ ] Tray icon, presets, start minimised, auto-start
- [ ] One-time setup that offers to install VB-Cable
- [ ] Optional chain item that hosts third-party plugins (CLAP / VST3)

## Virtual cable

Windows only lists recording devices that a driver provides, and unsigned drivers will
not load with Secure Boot on. middlemic does not ship its own driver. It plays into the
playback side of a signed virtual cable, [VB-CABLE](https://vb-audio.com/Cable/), and you
pick its recording side (`CABLE Output`) in Discord.

The **Set up virtual cable** button downloads VB-CABLE from vb-audio.com, checks the
installer's digital signature, and runs it with the usual Windows admin and driver-trust
prompts. Nothing from VB-Audio is stored in this repository.

VB-CABLE is donationware by VB-Audio ([www.vb-cable.com](https://vb-cable.com)). All
participations are welcome.

## Build

Cross-compiles from Linux or WSL:

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
cargo test
```

Or with make: `make build`, `make test`, and `make open`, which builds if anything changed
and then starts the window on Windows (WSL only). Each `make open` starts another copy, so
close the old window first.

## Use

```sh
middlemic.exe                  # opens the window
middlemic.exe devices          # list device names
middlemic.exe run --input "Headset Microphone" --output "CABLE Input"   # no window, saved settings
```

DeepFilterNet comes from a pinned git commit of
[Rikorose/DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) (MIT OR Apache-2.0),
because the crates.io release has no inference code.

## Testing with a recording

The tests include offline tools that run a real recording (anything `symphonia` can decode,
such as `.m4a`) through the chain in 10 ms frames, as the live engine does. They are
`#[ignore]`d, so run them by name:

```sh
export MIDDLEMIC_TEST_AUDIO=/path/to/recording.m4a MIDDLEMIC_TEST_OUT=/path/to/output-dir
cargo test --release analyse_chain -- --ignored --nocapture   # per-frame timing, gating
cargo test --release analyse_cap   -- --ignored --nocapture   # noise removal strength sweep
cargo test --release measure_delay -- --ignored --nocapture   # real delay of each stage
```

Processed `.wav` files are written to `MIDDLEMIC_TEST_OUT` so you can listen to the result.
When the output is lined up against the input, allow for the noise removal's 30 ms delay,
or every word edge looks cut.

## Licence

MIT OR Apache-2.0, at your option.
