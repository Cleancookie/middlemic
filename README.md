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
- Stages, in order: DeepFilterNet3 noise removal, mic boost, EQ, de-esser, 3-band compressor, limiter
- Simple window: device pickers, start/stop, level meters, per-stage on/off and sliders
- Settings and device choices are saved to `%APPDATA%\middlemic\settings.ini`
- Whole chain costs about 4% of one core in testing (DeepFilterNet about 1.6% of that),
  and the DSP thread runs at below-normal priority

Planned:

- [ ] Tray icon, presets, start minimised, auto-start
- [ ] One-time setup that offers to install VB-Cable
- [ ] Optional chain item that hosts third-party plugins (CLAP / VST3)

## Virtual cable

Windows only lists recording devices that a driver provides, and unsigned drivers will
not load with Secure Boot on. middlemic does not ship a driver. Use the playback side of
an existing signed virtual cable such as [VB-Cable](https://vb-audio.com/Cable/), then
pick its recording side in Discord. VB-Cable is not bundled or redistributed here.

## Build

Cross-compiles from Linux or WSL:

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
cargo test
```

## Use

```sh
middlemic.exe                  # opens the window
middlemic.exe devices          # list device names
middlemic.exe run --input "Headset Microphone" --output "CABLE Input"   # no window, saved settings
```

DeepFilterNet comes from a pinned git commit of
[Rikorose/DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) (MIT OR Apache-2.0),
because the crates.io release has no inference code.

## Licence

MIT OR Apache-2.0, at your option.
