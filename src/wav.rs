//! Reading and writing the mono 48 kHz WAV files that recordings are kept in.
//! Written as 32-bit float so a recording is exactly what the mic delivered.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const RATE: u32 = 48_000;
const HEADER_LEN: usize = 44;

fn header(data_bytes: u32) -> [u8; HEADER_LEN] {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend(b"RIFF");
    h.extend((36 + data_bytes).to_le_bytes());
    h.extend(b"WAVEfmt ");
    h.extend(16u32.to_le_bytes());
    h.extend(3u16.to_le_bytes()); // IEEE float
    h.extend(1u16.to_le_bytes()); // mono
    h.extend(RATE.to_le_bytes());
    h.extend((RATE * 4).to_le_bytes());
    h.extend(4u16.to_le_bytes());
    h.extend(32u16.to_le_bytes());
    h.extend(b"data");
    h.extend(data_bytes.to_le_bytes());
    h.try_into().expect("a WAV header is 44 bytes")
}

/// Appends samples to a file as they arrive. The sizes in the header are filled in by
/// [`Writer::finish`]; a file that was never finished is still readable.
pub struct Writer {
    file: BufWriter<File>,
    bytes: u32,
}

impl Writer {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(0))?;
        Ok(Self { file, bytes: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for s in samples {
            self.file.write_all(&s.to_le_bytes())?;
        }
        self.bytes = self.bytes.saturating_add(samples.len() as u32 * 4);
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.bytes))?;
        self.file.flush()
    }
}

pub fn write_mono(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let mut writer = Writer::create(path)?;
    writer.write(samples)?;
    writer.finish()
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Mono samples and the sample rate. Reads 16-bit PCM and 32-bit float, mixing channels down.
pub fn read_mono(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let mut bytes = Vec::new();
    File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut format, mut at) = (None, 12);
    while at + 8 <= bytes.len() {
        let (id, size) = (&bytes[at..at + 4], u32_at(&bytes, at + 4) as usize);
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            format = Some((u16_at(&bytes, body), u16_at(&bytes, body + 2) as usize, u32_at(&bytes, body + 4), u16_at(&bytes, body + 14)));
        } else if id == b"data" {
            // A recording cut short by a crash still says 0 bytes; take what is there.
            let end = if size == 0 || body + size > bytes.len() { bytes.len() } else { body + size };
            let (tag, channels, rate, bits) = format.ok_or("the WAV has no format chunk")?;
            let data = &bytes[body..end];
            let samples: Vec<f32> = match (tag, bits) {
                (3, 32) => data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
                (1, 16) => data.chunks_exact(2).map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0).collect(),
                _ => return Err(format!("unsupported WAV format (tag {tag}, {bits} bits)")),
            };
            let channels = channels.max(1);
            let mono = samples.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect();
            return Ok((mono, rate));
        }
        at = body + size + (size & 1);
    }
    Err("the WAV has no audio".into())
}

/// Length of a file written by this module, from its size alone, or `None` if it is not one.
pub fn seconds(path: &Path) -> Option<f32> {
    let mut head = [0u8; HEADER_LEN];
    File::open(path).ok()?.read_exact(&mut head).ok()?;
    let own_format = head[..4] == *b"RIFF" && u16_at(&head, 20) == 3 && u16_at(&head, 22) == 1 && u32_at(&head, 24) == RATE;
    if !own_format {
        return None;
    }
    let audio_bytes = std::fs::metadata(path).ok()?.len().saturating_sub(HEADER_LEN as u64);
    Some(audio_bytes as f32 / 4.0 / RATE as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("middlemic-wav-{name}-{}.wav", std::process::id()))
    }

    #[test]
    fn what_is_written_reads_back_exactly() {
        let path = temp("round");
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        write_mono(&path, &samples).unwrap();
        let (back, rate) = read_mono(&path).unwrap();
        assert_eq!((back, rate), (samples, RATE));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn length_comes_from_the_file_size() {
        let path = temp("length");
        write_mono(&path, &vec![0.0; 24_000]).unwrap();
        assert_eq!(seconds(&path), Some(0.5));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_recording_that_was_never_finished_still_plays() {
        let path = temp("cut");
        let mut writer = Writer::create(&path).unwrap();
        writer.write(&[0.25; 480]).unwrap();
        drop(writer); // never finished: the header says 0 bytes of audio
        let (back, _) = read_mono(&path).unwrap();
        assert_eq!(back.len(), 480);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stereo_16_bit_files_mix_down() {
        let path = temp("pcm");
        let mut data = Vec::new();
        for (l, r) in [(16384i16, 0i16), (0, -16384)] {
            data.extend(l.to_le_bytes());
            data.extend(r.to_le_bytes());
        }
        let mut file = Vec::new();
        file.extend(b"RIFF");
        file.extend((36 + data.len() as u32).to_le_bytes());
        file.extend(b"WAVEfmt ");
        file.extend(16u32.to_le_bytes());
        file.extend(1u16.to_le_bytes());
        file.extend(2u16.to_le_bytes());
        file.extend(RATE.to_le_bytes());
        file.extend((RATE * 4).to_le_bytes());
        file.extend(4u16.to_le_bytes());
        file.extend(16u16.to_le_bytes());
        file.extend(b"data");
        file.extend((data.len() as u32).to_le_bytes());
        file.extend(data);
        std::fs::write(&path, file).unwrap();
        let (mono, _) = read_mono(&path).unwrap();
        assert_eq!(mono, [0.25, -0.25]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn something_else_is_refused() {
        let path = temp("junk");
        std::fs::write(&path, b"not a wav at all").unwrap();
        assert!(read_mono(&path).is_err());
        assert_eq!(seconds(&path), None);
        let _ = std::fs::remove_file(&path);
    }
}
