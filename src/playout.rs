//! What the output thread plays each time the sound card asks for audio.
//!
//! The capture and output devices run on different clocks and the processing thread has
//! its own timing, so audio arrives in uneven bursts. With no cushion, every late burst
//! leaves a gap. This holds back a little audio before starting, and again after running
//! dry, so ordinary jitter is absorbed instead of heard.

use ringbuf::traits::Consumer;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Fill {
    /// Real audio, no gap.
    Played,
    /// Still building up the cushion; played silence on purpose.
    Priming,
    /// Ran out mid-fill: part of this fill is silence. This is a dropout.
    RanDry,
}

pub struct Playout {
    cushion: usize,
    primed: bool,
}

impl Playout {
    /// `cushion` is how many samples must be waiting before playback starts.
    pub fn new(cushion: usize) -> Self {
        Self { cushion, primed: false }
    }

    pub fn fill(&mut self, ring: &mut impl Consumer<Item = f32>, out: &mut [f32]) -> Fill {
        if !self.primed {
            if ring.occupied_len() < self.cushion {
                out.fill(0.0);
                return Fill::Priming;
            }
            self.primed = true;
        }
        let got = ring.pop_slice(out);
        if got < out.len() {
            out[got..].fill(0.0);
            self.primed = false;
            return Fill::RanDry;
        }
        Fill::Played
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;
    use ringbuf::traits::{Observer, Producer, Split};

    #[test]
    fn waits_for_the_cushion_before_playing() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(4096).split();
        let mut playout = Playout::new(400);
        prod.push_slice(&[0.5; 100]);
        let mut out = [1.0; 50];
        assert_eq!(playout.fill(&mut cons, &mut out), Fill::Priming);
        assert!(out.iter().all(|&s| s == 0.0), "silence while priming");
        assert_eq!(cons.occupied_len(), 100, "nothing consumed while priming");

        prod.push_slice(&[0.5; 300]);
        assert_eq!(playout.fill(&mut cons, &mut out), Fill::Played);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn plays_audio_in_order() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(64).split();
        let mut playout = Playout::new(0);
        prod.push_slice(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0; 4];
        assert_eq!(playout.fill(&mut cons, &mut out), Fill::Played);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn running_dry_plays_what_it_has_then_rebuilds_the_cushion() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(4096).split();
        let mut playout = Playout::new(200);
        prod.push_slice(&[0.7; 300]);
        let mut out = [0.0; 400];
        assert_eq!(playout.fill(&mut cons, &mut out), Fill::RanDry);
        assert!(out[..300].iter().all(|&s| s == 0.7) && out[300..].iter().all(|&s| s == 0.0));

        prod.push_slice(&[0.9; 100]);
        let mut small = [0.0; 50];
        assert_eq!(playout.fill(&mut cons, &mut small), Fill::Priming, "100 < the 200 cushion");
        prod.push_slice(&[0.9; 150]);
        assert_eq!(playout.fill(&mut cons, &mut small), Fill::Played);
    }

    /// 60 seconds of uneven arrivals (each 10 ms packet up to 4 ms early or late) against a
    /// steady 10 ms consumer, plus a 25 ms stall every 2 seconds, like a game briefly
    /// starving the audio thread. Returns how many times the consumer ran dry.
    ///
    /// Plain jitter alone is not enough to hear: after one late packet the queue keeps a
    /// packet of slack and heals itself. Stalls longer than that slack are what leave gaps.
    fn dropouts_with_cushion(cushion: usize) -> usize {
        const PACKET: usize = 960; // 10 ms of stereo
        let (mut prod, mut cons) = HeapRb::<f32>::new(PACKET * 40).split();
        let mut playout = Playout::new(cushion);
        let mut seed = 12345u32;
        let mut jitter = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 16) as i32 % 9 - 4 // -4..=4 ms
        };
        // (time in ms, is_producer)
        let mut events: Vec<(i32, bool)> = Vec::new();
        for k in 0..6000 {
            // Every 2 s the producer is held for 25 ms; the packets that were due meanwhile
            // are delivered together afterwards.
            let into_stall = k % 200 - 100;
            let arrival = if (0..3).contains(&into_stall) {
                (k - into_stall) * 10 + 20 + 25 + into_stall
            } else {
                k * 10 + 20 + jitter()
            };
            events.push((arrival, true));
            // The card asks at its own steady cadence. With no deliberate slack in the design,
            // that lines up with the average arrival time, so jitter decides who is first.
            events.push((k * 10 + 20, false));
        }
        events.sort();
        let (mut dry, mut out) = (0, vec![0.0; PACKET]);
        for (_, produce) in events {
            if produce {
                prod.push_slice(&[0.3; PACKET]);
            } else if playout.fill(&mut cons, &mut out) == Fill::RanDry {
                dry += 1;
            }
        }
        dry
    }

    #[test]
    fn a_cushion_absorbs_the_jitter_that_causes_gaps_without_one() {
        let without = dropouts_with_cushion(0);
        let with = dropouts_with_cushion(5 * 960);
        assert!(without > 0, "no cushion should hear the stalls, got {without}");
        assert_eq!(with, 0, "50 ms cushion: no gaps");
    }
}
