//! Little-endian `f32` PCM <-> bytes, as WASAPI hands and takes them.

/// Appends the whole `f32` samples in `bytes` to `out`; a trailing partial sample is ignored.
pub fn append_samples(bytes: &[u8], out: &mut Vec<f32>) {
    out.extend(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
}

/// Replaces the contents of `out` with `samples` as bytes.
pub fn write_bytes(samples: &[f32], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_survive_a_round_trip_through_bytes() {
        let samples = [0.0, -0.0, 1.0, -1.0, 0.123_456_79, f32::MIN_POSITIVE, f32::MAX];
        let (mut bytes, mut back) = (Vec::new(), Vec::new());
        write_bytes(&samples, &mut bytes);
        assert_eq!(bytes.len(), samples.len() * 4);
        append_samples(&bytes, &mut back);
        assert_eq!(back, samples);
    }

    #[test]
    fn bytes_are_little_endian_and_a_partial_sample_is_dropped() {
        let mut out = Vec::new();
        append_samples(&[0x00, 0x00, 0x80, 0x3f, 0xaa, 0xbb], &mut out);
        assert_eq!(out, [1.0]);
    }

    #[test]
    fn write_bytes_replaces_old_contents() {
        let mut bytes = vec![9; 3];
        write_bytes(&[1.0], &mut bytes);
        assert_eq!(bytes, 1.0f32.to_le_bytes());
    }
}
