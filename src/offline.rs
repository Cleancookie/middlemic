//! Test-only tools for running real recordings through the chain offline: decode a file,
//! feed it through the stages frame by frame like the live engine, and measure.
//! Nothing here ships in the app.

use std::path::Path;
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub struct Clip {
    pub mono: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Decode any file symphonia understands to mono `f32`.
pub fn decode(path: &Path) -> Clip {
    let file = Box::new(std::fs::File::open(path).expect("open the recording"));
    let mss = MediaSourceStream::new(file, Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(&Hint::new(), mss, FormatOptions::default(), MetadataOptions::default())
        .expect("recognise the format");
    let track = format.default_track(TrackType::Audio).expect("an audio track");
    let track_id = track.id;
    let params = track.codec_params.as_ref().unwrap().audio().unwrap().clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("a decoder");

    let (mut interleaved, mut mono) = (Vec::<f32>::new(), Vec::<f32>::new());
    let (mut channels, mut sample_rate) = (0usize, 0u32);
    while let Some(packet) = format.next_packet().expect("read packets") {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                channels = buf.spec().channels().count();
                sample_rate = buf.spec().rate();
                interleaved.resize(buf.samples_interleaved(), f32::MID);
                buf.copy_to_slice_interleaved(&mut interleaved);
                mono.extend(interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
            }
            Err(Error::DecodeError(_)) => {}
            Err(e) => panic!("decode failed: {e}"),
        }
    }
    Clip { mono, sample_rate, channels }
}

/// Cubic-free linear resample: good enough to put a recording at the app's 48 kHz.
pub fn to_48k(clip: &Clip) -> Vec<f32> {
    if clip.sample_rate == 48_000 {
        return clip.mono.clone();
    }
    let ratio = clip.sample_rate as f64 / 48_000.0;
    let n = (clip.mono.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let (a, t) = (pos.floor() as usize, pos.fract() as f32);
            let b = (a + 1).min(clip.mono.len() - 1);
            clip.mono[a] * (1.0 - t) + clip.mono[b] * t
        })
        .collect()
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(sample_rate.to_le_bytes());
    out.extend((sample_rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    std::fs::write(path, out).expect("write the wav");
}

/// RMS level in dBFS of each 10 ms frame.
pub fn frame_levels_db(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks_exact(480)
        .map(|f| 20.0 * (f.iter().map(|s| s * s).sum::<f32>() / 480.0).sqrt().max(1e-6).log10())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_writer_makes_a_valid_header() {
        let dir = std::env::temp_dir().join(format!("middlemic-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav(&path, &[0.0, 0.5, -0.5], 48_000);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resampling_keeps_duration() {
        let clip = Clip { mono: vec![0.1; 44_100], sample_rate: 44_100, channels: 1 };
        assert!((to_48k(&clip).len() as i64 - 48_000).abs() <= 1);
    }

    fn percentile(sorted: &[f32], p: f32) -> f32 {
        sorted[((sorted.len() - 1) as f32 * p) as usize]
    }

    /// Runs `clip` through a chain in 10 ms frames, like the live engine. Returns the output
    /// and how long each frame took, in milliseconds.
    fn run(clip: &[f32], settings: &crate::settings::Settings) -> (Vec<f32>, Vec<f32>) {
        let mut chain = crate::dsp::build_chain(settings).unwrap();
        chain.prepare(48_000);
        let mut out = clip[..clip.len() / 480 * 480].to_vec();
        let mut times = Vec::with_capacity(out.len() / 480);
        for frame in out.chunks_exact_mut(480) {
            let started = std::time::Instant::now();
            chain.process(frame);
            times.push(started.elapsed().as_secs_f32() * 1000.0);
        }
        (out, times)
    }

    fn only(settings: &crate::settings::Settings, keep: &[&str]) {
        for s in &settings.sections {
            s.enabled.store(keep.contains(&s.name), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Speech frames are those well above the noise floor. A dropout is a speech frame the
    /// chain turned down by more than 15 dB. Counts dropouts and how many separate runs.
    fn dropouts(input: &[f32], output: &[f32]) -> (usize, usize, usize, f32) {
        let (i, o) = (frame_levels_db(input), frame_levels_db(output));
        let mut sorted = i.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let floor = percentile(&sorted, 0.10);
        let speech: Vec<bool> = i.iter().map(|&l| l > floor + 20.0).collect();
        let (mut frames, mut runs, mut was) = (0, 0, false);
        let mut loss = 0.0;
        let mut speech_frames = 0;
        for k in 0..i.len() {
            if !speech[k] {
                was = false;
                continue;
            }
            speech_frames += 1;
            loss += i[k] - o[k];
            let dropped = o[k] < i[k] - 15.0;
            if dropped {
                frames += 1;
                if !was {
                    runs += 1;
                }
            }
            was = dropped;
        }
        (frames, runs, speech_frames, loss / speech_frames.max(1) as f32)
    }

    /// `MIDDLEMIC_TEST_AUDIO=<file> MIDDLEMIC_TEST_OUT=<dir> cargo test --release analyse_chain -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn analyse_chain() {
        let clip = decode(Path::new(&std::env::var("MIDDLEMIC_TEST_AUDIO").unwrap()));
        let input = to_48k(&clip);
        let out_dir = std::env::var("MIDDLEMIC_TEST_OUT").ok();
        let mut sorted_in = frame_levels_db(&input);
        sorted_in.sort_by(|a, b| a.total_cmp(b));
        println!(
            "input levels: floor {:.0} dB, median {:.0} dB, loud {:.0} dB",
            percentile(&sorted_in, 0.1),
            percentile(&sorted_in, 0.5),
            percentile(&sorted_in, 0.95)
        );

        type Setup = Box<dyn Fn(&crate::settings::Settings)>;
        let cases: Vec<(&str, Setup)> = vec![
            (
                "noise removal only, full strength",
                Box::new(|s| {
                    only(s, &["Noise removal"]);
                    s.section("Noise removal").params[0].set(100.0);
                }),
            ),
            (
                "noise removal only, capped at 30 dB",
                Box::new(|s| {
                    only(s, &["Noise removal"]);
                    s.section("Noise removal").params[0].set(30.0);
                }),
            ),
            ("everything except noise removal", Box::new(|s| {
                let all: Vec<&str> = s.sections.iter().map(|x| x.name).filter(|n| *n != "Noise removal").collect();
                only(s, &all);
            })),
            ("full default chain", Box::new(|_| {})),
        ];
        for (name, setup) in cases {
            let settings = crate::settings::Settings::new(crate::dsp::default_sections());
            setup(&settings);
            let (output, times) = run(&input, &settings);
            let mut t = times[20..].to_vec(); // skip warm-up
            t.sort_by(|a, b| a.total_cmp(b));
            let over = |ms: f32| t.iter().filter(|&&x| x > ms).count();
            let (frames, runs, speech, mean_loss) = dropouts(&input[..output.len()], &output);
            println!("\n== {name}");
            println!(
                "  per-frame time ms: mean {:.2}  p50 {:.2}  p99 {:.2}  p99.9 {:.2}  max {:.2}  (first 20 frames max {:.2})",
                t.iter().sum::<f32>() / t.len() as f32,
                percentile(&t, 0.5),
                percentile(&t, 0.99),
                percentile(&t, 0.999),
                t.last().unwrap(),
                times[..20].iter().cloned().fold(0.0, f32::max)
            );
            println!("  frames over 5 ms: {}   over 10 ms (a missed deadline): {}   of {}", over(5.0), over(10.0), t.len());
            println!(
                "  speech frames: {speech}   turned down >15 dB: {frames} in {runs} separate runs   average change in speech: -{mean_loss:.1} dB"
            );
            if let Some(dir) = &out_dir {
                let file = format!("{}/{}.wav", dir, name.replace([' ', ','], "_"));
                write_wav(Path::new(&file), &output, 48_000);
            }
        }
    }

    /// How many samples later `output` is than `input`, by brute-force cross-correlation of a
    /// few seconds of speech. Noise removal delays its output, and comparing without
    /// allowing for that makes every word edge look cut.
    fn measure_lag(input: &[f32], output: &[f32]) -> usize {
        let (from, len) = (48_000 * 2, 48_000 * 4);
        let reference = &input[from..from + len];
        (0..=4_800)
            .map(|lag| {
                let score: f32 = reference
                    .iter()
                    .zip(&output[from + lag..from + lag + len])
                    .map(|(a, b)| a * b)
                    .sum();
                (lag, score)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap()
            .0
    }

    /// The delay the chain adds, measured rather than estimated.
    /// `MIDDLEMIC_TEST_AUDIO=<file> cargo test --release measure_delay -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure_delay() {
        let clip = decode(Path::new(&std::env::var("MIDDLEMIC_TEST_AUDIO").unwrap()));
        let input = to_48k(&clip);
        for (name, keep) in [("noise removal only", vec!["Noise removal"]), ("everything else", vec!["Mic boost", "EQ", "De-esser", "Compressor", "Limiter"])] {
            let settings = crate::settings::Settings::new(crate::dsp::default_sections());
            only(&settings, &keep);
            let (output, _) = run(&input, &settings);
            let lag = measure_lag(&input, &output);
            println!("{name}: output is {lag} samples = {:.1} ms later than the input", lag as f32 / 48.0);
        }
    }

    /// How much noise removal can we allow before it eats speech?
    /// `MIDDLEMIC_TEST_AUDIO=<file> MIDDLEMIC_TEST_OUT=<dir> cargo test --release analyse_cap -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn analyse_cap() {
        let clip = decode(Path::new(&std::env::var("MIDDLEMIC_TEST_AUDIO").unwrap()));
        let input = to_48k(&clip);
        let out_dir = std::env::var("MIDDLEMIC_TEST_OUT").ok();
        let mut sorted = frame_levels_db(&input);
        sorted.sort_by(|a, b| a.total_cmp(b));
        let floor = percentile(&sorted, 0.10);

        println!("(output lined up with the input; max reduction | speech frames cut >10 dB | worst 10 ms of speech | avg speech change | avg background change)");
        for cap in [100.0f32, 40.0, 25.0, 18.0, 12.0, 8.0] {
            let settings = crate::settings::Settings::new(crate::dsp::default_sections());
            only(&settings, &["Noise removal"]);
            settings.section("Noise removal").params[0].set(cap);
            let (output, _) = run(&input, &settings);
            let lag = measure_lag(&input, &output);
            let n = output.len() - lag;
            let (i, o) = (frame_levels_db(&input[..n]), frame_levels_db(&output[lag..lag + n]));

            let (mut cut, mut speech, mut worst, mut speech_sum) = (0, 0, 0f32, 0f32);
            let (mut bg, mut bg_sum) = (0, 0f32);
            for k in 0..o.len() {
                let change = i[k] - o[k];
                if i[k] > floor + 15.0 {
                    speech += 1;
                    speech_sum += change;
                    worst = worst.max(change);
                    if change > 10.0 {
                        cut += 1;
                    }
                } else if i[k] < floor + 6.0 {
                    bg += 1;
                    bg_sum += change;
                }
            }
            println!(
                "  {cap:>5.0} dB    | {cut:>3} of {speech}            | -{worst:>5.1} dB            | -{:>4.1} dB          | -{:>4.1} dB",
                speech_sum / speech as f32,
                bg_sum / bg.max(1) as f32
            );
            if let Some(dir) = &out_dir {
                write_wav(Path::new(&format!("{dir}/cap_{cap:.0}_dB.wav")), &output, 48_000);
            }
        }
    }

    /// Lists each burst of sound with how much noise removal turned it down.
    /// `MIDDLEMIC_TEST_AUDIO=<file> cargo test --release list_segments -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn list_segments() {
        let clip = decode(Path::new(&std::env::var("MIDDLEMIC_TEST_AUDIO").unwrap()));
        let input = to_48k(&clip);
        let settings = crate::settings::Settings::new(crate::dsp::default_sections());
        only(&settings, &["Noise removal"]);
        let (output, _) = run(&input, &settings);
        let (i, o) = (frame_levels_db(&input), frame_levels_db(&output));
        let mut sorted = i.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let threshold = percentile(&sorted, 0.10) + 15.0;

        println!("start   length  in dB   out dB  change   (bursts above {threshold:.0} dB, gaps under 100 ms merged)");
        let mut k = 0;
        while k < i.len() {
            if i[k] <= threshold {
                k += 1;
                continue;
            }
            let start = k;
            let mut end = k;
            while end + 1 < i.len() && (end + 1..(end + 11).min(i.len())).any(|j| i[j] > threshold) {
                end += 1;
            }
            let span = start..=end;
            let mean = |v: &[f32]| v[span.clone()].iter().sum::<f32>() / (end - start + 1) as f32;
            let peak = |v: &[f32]| v[span.clone()].iter().cloned().fold(-120.0, f32::max);
            println!(
                "{:>5.2}s {:>5} ms  {:>5.1}  {:>6.1}  {:>+6.1} dB   (loudest 10 ms: in {:.0}, out {:.0})",
                start as f32 * 0.01,
                (end - start + 1) * 10,
                mean(&i),
                mean(&o),
                mean(&o) - mean(&i),
                peak(&i),
                peak(&o)
            );
            k = end + 1;
        }
    }

    /// Does noise removal chop speech less when the mic is boosted first?
    /// `MIDDLEMIC_TEST_AUDIO=<file> MIDDLEMIC_TEST_OUT=<dir> cargo test --release analyse_level -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn analyse_level() {
        let clip = decode(Path::new(&std::env::var("MIDDLEMIC_TEST_AUDIO").unwrap()));
        let input = to_48k(&clip);
        let out_dir = std::env::var("MIDDLEMIC_TEST_OUT").ok();
        println!("boost before noise removal | speech chopped >15 dB (frames in runs) | speech change | background change");
        for boost_db in [0.0f32, 6.0, 12.0, 18.0, 24.0] {
            let gain = 10f32.powf(boost_db / 20.0);
            let boosted: Vec<f32> = input.iter().map(|s| (s * gain).clamp(-1.0, 1.0)).collect();
            let settings = crate::settings::Settings::new(crate::dsp::default_sections());
            only(&settings, &["Noise removal"]);
            let (output, _) = run(&boosted, &settings);
            let (frames, runs, speech, speech_change) = dropouts(&boosted[..output.len()], &output);

            // Background = frames near the noise floor of the boosted input.
            let (i, o) = (frame_levels_db(&boosted), frame_levels_db(&output));
            let mut sorted = i.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let floor = percentile(&sorted, 0.10);
            let quiet: Vec<f32> = i.iter().zip(&o).filter(|(l, _)| **l < floor + 6.0).map(|(a, b)| a - b).collect();
            let background = quiet.iter().sum::<f32>() / quiet.len().max(1) as f32;
            println!(
                "  +{boost_db:>4.0} dB | {frames:>3} of {speech} frames in {runs:>2} runs | -{speech_change:>4.1} dB | -{background:>4.1} dB"
            );
            if let (Some(dir), true) = (&out_dir, boost_db == 18.0) {
                write_wav(Path::new(&format!("{dir}/boost_18_then_noise_removal.wav")), &output, 48_000);
            }
        }
    }

    /// `MIDDLEMIC_TEST_AUDIO=<file> cargo test --release inspect_recording -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn inspect_recording() {
        let path = std::env::var("MIDDLEMIC_TEST_AUDIO").expect("set MIDDLEMIC_TEST_AUDIO");
        let clip = decode(Path::new(&path));
        let peak = clip.mono.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!(
            "{} Hz, {} ch, {:.1} s, peak {:.2} ({:.1} dBFS)",
            clip.sample_rate,
            clip.channels,
            clip.mono.len() as f32 / clip.sample_rate as f32,
            peak,
            20.0 * peak.log10()
        );
    }
}
