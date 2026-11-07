//! Platform-independent DSP. Every stage implements [`Processor`]; a [`Chain`]
//! runs them in order, so stages can be reordered, swapped or duplicated.

mod biquad;
mod cap;
mod compressor;
mod dpdfnet;
mod deepfilter;
mod deesser;
mod echo;
mod eq;
mod gain;
mod limiter;
pub mod multiband;
mod rnnoise;

pub use deepfilter::DeepFilter;
#[allow(unused_imports)] // The download button is in the Windows-only window.
pub use dpdfnet::{DpdfNet, LIGHT as DPDFNET, download_dir as model_download_dir, model_installed};
pub use echo::{EchoCancel, Reference};
pub use deesser::DeEsser;
#[allow(unused_imports)] // EQ_NAME is used by the Windows-only window.
pub use eq::{BANDS, Eq, NAME as EQ_NAME, response_db};
pub use gain::Gain;
pub use limiter::Limiter;
pub use multiband::Multiband;
pub use rnnoise::RnNoise;

use crate::settings::{Item, Section, Settings};
use anyhow::Result;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Every kind of stage with a maker for its controls, in the default rack's order.
const KINDS: [(&str, fn() -> Section); 9] = [
    (echo::NAME, echo::section),
    (deepfilter::NAME, deepfilter::section),
    (dpdfnet::LIGHT.name, || dpdfnet::section(dpdfnet::LIGHT)),
    (rnnoise::NAME, rnnoise::section),
    (gain::NAME, gain::section),
    (eq::NAME, eq::section),
    (deesser::NAME, deesser::section),
    (multiband::NAME, multiband::section),
    (limiter::NAME, limiter::section),
];

/// The names of every kind of stage, in the default rack's order.
pub fn kinds() -> impl Iterator<Item = &'static str> {
    KINDS.iter().map(|(name, _)| *name)
}

/// Fresh controls for a stage of `kind`, or `None` if no such kind exists.
pub fn new_section(kind: &str) -> Option<Section> {
    KINDS.iter().find(|(name, _)| *name == kind).map(|(_, make)| make())
}

/// One stage of each kind, as a fresh install starts.
pub fn default_sections() -> Vec<Section> {
    KINDS.iter().map(|(_, make)| make()).collect()
}

/// The frequency of the first enabled de-esser after the stage `id`, if any: where the EQ
/// panel marks that the de-esser starts.
pub fn deesser_after(items: &[Arc<Item>], id: u64) -> Option<f32> {
    items
        .iter()
        .skip_while(|i| i.id != id)
        .skip(1)
        .find(|i| i.kind() == deesser::NAME && i.section.is_enabled())
        .map(|i| deesser::frequency_hz(&i.section))
}

/// The DSP for one stage, ready to run at `sample_rate`. `reference` is the audio going to the
/// speakers, for echo cancellation. Loading a model can take a few hundred milliseconds.
fn build_stage(section: Arc<Section>, reference: Option<Reference>, sample_rate: u32) -> Result<Box<dyn Processor>> {
    let mut stage: Box<dyn Processor> = match section.name {
        echo::NAME => Box::new(EchoCancel::new(section, reference)),
        name if name == dpdfnet::LIGHT.name => Box::new(DpdfNet::new(section, dpdfnet::LIGHT)),
        rnnoise::NAME => Box::new(RnNoise::new(section)),
        deepfilter::NAME => Box::new(DeepFilter::new(section)?),
        gain::NAME => Box::new(Gain::new(section)),
        eq::NAME => Box::new(Eq::new(section)),
        deesser::NAME => Box::new(DeEsser::new(section)),
        multiband::NAME => Box::new(Multiband::new(section)),
        limiter::NAME => Box::new(Limiter::new(section)),
        other => anyhow::bail!("no stage is known for the section {other:?}"),
    };
    stage.prepare(sample_rate);
    Ok(stage)
}

/// A chain of every stage in the rack, in running order, prepared for `sample_rate`. With
/// `reference: None` echo cancellation never runs.
pub fn build_chain(settings: &Settings, reference: Option<Reference>, sample_rate: u32) -> Result<Chain> {
    let mut chain = Chain::new();
    for item in settings.items() {
        let stage = build_stage(item.section.clone(), reference.clone(), sample_rate)?;
        chain.stages.push((item.id, stage));
    }
    Ok(chain)
}

/// One stage of the mic chain. Mono `f32` samples, processed in place.
pub trait Processor: Send {
    /// A disabled stage is skipped entirely.
    fn enabled(&self) -> bool {
        true
    }

    /// Called once before processing, and again if the sample rate changes.
    fn prepare(&mut self, _sample_rate: u32) {}

    fn process(&mut self, samples: &mut [f32]);

    /// Extra delay this stage adds, in samples.
    fn latency_samples(&self) -> usize {
        0
    }
}

/// Stages in running order, each tagged with the id of the rack item it belongs to.
#[derive(Default)]
pub struct Chain {
    stages: Vec<(u64, Box<dyn Processor>)>,
}

impl Chain {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn push(&mut self, id: u64, processor: impl Processor + 'static) -> &mut Self {
        self.stages.push((id, Box::new(processor)));
        self
    }

    fn has(&self, id: u64) -> bool {
        self.stages.iter().any(|(i, _)| *i == id)
    }

    /// Run the stages of `ids` in that order, taking each from this chain or from `new`.
    /// Stages already here keep their state; those not in `ids` are dropped, and ids with
    /// no stage are skipped.
    pub fn arrange(&mut self, ids: &[u64], new: Vec<(u64, Box<dyn Processor>)>) {
        let mut pool: Vec<_> = self.stages.drain(..).chain(new).map(Some).collect();
        for &id in ids {
            let found = pool.iter_mut().find(|s| s.as_ref().is_some_and(|(i, _)| *i == id));
            if let Some(stage) = found.and_then(Option::take) {
                self.stages.push(stage);
            }
        }
    }

    #[cfg(test)]
    pub fn prepare(&mut self, sample_rate: u32) {
        for (_, stage) in &mut self.stages {
            stage.prepare(sample_rate);
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for (_, stage) in &mut self.stages {
            if stage.enabled() {
                stage.process(samples);
            }
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.stages
            .iter()
            .filter(|(_, s)| s.enabled())
            .map(|(_, s)| s.latency_samples())
            .sum()
    }
}

/// Stages built off the audio thread for the rack as it was at `generation`.
struct Built {
    generation: u64,
    ids: Vec<u64>,
    stages: Vec<(u64, Box<dyn Processor>)>,
    failed: Vec<u64>,
}

/// A chain that follows the rack while it runs. Reordering and removing happen between two
/// blocks. A stage that is added is built on a helper thread, since loading a model takes
/// longer than the audio can wait; until it is ready the chain carries on as it was.
pub struct LiveChain {
    chain: Chain,
    settings: Arc<Settings>,
    reference: Option<Reference>,
    sample_rate: u32,
    /// The rack generation the chain matches.
    seen: u64,
    building: Option<(u64, Receiver<Built>)>,
    /// Items whose stage would not build. Not retried, or a broken one would rebuild every block.
    failed: Vec<u64>,
}

impl LiveChain {
    /// Builds the whole chain on the calling thread.
    pub fn new(settings: Arc<Settings>, reference: Option<Reference>, sample_rate: u32) -> Result<Self> {
        let seen = settings.generation();
        let chain = build_chain(&settings, reference.clone(), sample_rate)?;
        Ok(Self { chain, settings, reference, sample_rate, seen, building: None, failed: Vec::new() })
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        self.follow();
        self.chain.process(samples);
    }

    pub fn latency_samples(&self) -> usize {
        self.chain.latency_samples()
    }

    /// Cheap unless the rack changed or a build finished.
    fn follow(&mut self) {
        if let Some((generation, building)) = &self.building {
            match building.try_recv() {
                Ok(built) => {
                    self.seen = built.generation;
                    self.failed.extend(built.failed);
                    self.chain.arrange(&built.ids, built.stages);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.seen = *generation,
            }
            self.building = None;
        }
        if self.settings.generation() == self.seen {
            return;
        }
        let (generation, items) = self.settings.snapshot();
        let ids: Vec<u64> = items.iter().map(|i| i.id).collect();
        let missing: Vec<Arc<Item>> = items
            .into_iter()
            .filter(|i| !self.chain.has(i.id) && !self.failed.contains(&i.id))
            .collect();
        if missing.is_empty() {
            self.seen = generation;
            self.chain.arrange(&ids, Vec::new());
            return;
        }
        let (send, receive) = mpsc::channel();
        let (reference, sample_rate) = (self.reference.clone(), self.sample_rate);
        let spawned = std::thread::Builder::new().name("stage-builder".into()).spawn(move || {
            let (mut stages, mut failed) = (Vec::new(), Vec::new());
            for item in missing {
                match build_stage(item.section.clone(), reference.clone(), sample_rate) {
                    Ok(stage) => stages.push((item.id, stage)),
                    Err(err) => {
                        item.section.set_status(format!("{err:#}"));
                        failed.push(item.id);
                    }
                }
            }
            let _ = send.send(Built { generation, ids, stages, failed });
        });
        match spawned {
            Ok(_) => self.building = Some((generation, receive)),
            Err(_) => self.seen = generation,
        }
    }
}

#[cfg(test)]
pub mod testutil {
    pub fn sine(freq: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin())
            .collect()
    }

    pub fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};

    fn boosted(db: f32) -> Gain {
        let s = Arc::new(gain::section());
        s.params[0].set(db);
        Gain::new(s)
    }

    #[test]
    fn stages_run_in_order() {
        let ceiling = Arc::new(limiter::section());
        ceiling.params[0].set(-6.0);
        let mut chain = Chain::new();
        chain.push(0, boosted(6.0)).push(1, Limiter::new(ceiling));
        chain.prepare(48_000);
        let mut buf = vec![0.5_f32; 4_800];
        chain.process(&mut buf);
        let max = 10f32.powf(-6.0 / 20.0);
        assert!(buf.iter().all(|s| s.abs() <= max + 1e-6));
    }

    #[test]
    fn disabled_stage_is_skipped() {
        let s = Arc::new(gain::section());
        s.params[0].set(20.0);
        s.enabled.store(false, Relaxed);
        let mut chain = Chain::new();
        chain.push(0, Gain::new(s));
        let mut buf = vec![0.1_f32; 8];
        chain.process(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.1).abs() < 1e-9));
    }

    #[test]
    fn same_stage_can_be_used_twice() {
        let mut chain = Chain::new();
        chain.push(0, boosted(6.0)).push(1, boosted(6.0));
        chain.prepare(48_000);
        let mut buf = vec![0.1_f32; 48_000];
        chain.process(&mut buf);
        let expected = 0.1 * 10f32.powf(12.0 / 20.0);
        assert!((buf[47_999] - expected).abs() < 1e-3);
    }

    #[test]
    fn order_decides_what_the_audio_does() {
        let ceiling = Arc::new(limiter::section());
        ceiling.params[0].set(-6.0); // about 0.5
        let mut chain = Chain::new();
        chain.push(0, boosted(6.0)).push(1, Limiter::new(ceiling));
        chain.prepare(48_000);

        let mut limited_last = vec![0.4_f32; 48_000];
        chain.process(&mut limited_last);
        assert!(limited_last[47_999] <= 0.51, "boost then limit stays under the ceiling");

        // The limiter keeps its state across the change and takes a moment to ease off.
        chain.arrange(&[1, 0], Vec::new());
        let mut limited_first = vec![0.4_f32; 48_000];
        chain.process(&mut limited_first);
        assert!(limited_first[47_999] > 0.75, "limit then boost is louder: {}", limited_first[47_999]);
    }

    fn ids(chain: &Chain) -> Vec<u64> {
        chain.stages.iter().map(|(id, _)| *id).collect()
    }

    #[test]
    fn arranging_drops_missing_stages_and_skips_unknown_ids() {
        let mut chain = Chain::new();
        chain.push(1, boosted(0.0)).push(2, boosted(0.0)).push(3, boosted(0.0));
        chain.arrange(&[3, 9, 1], vec![(4, Box::new(boosted(0.0)))]);
        assert_eq!(ids(&chain), [3, 1], "2 and the unasked-for 4 dropped, 9 has no stage");
    }

    #[test]
    fn the_built_chain_follows_the_rack_order() {
        let settings = Settings::new(default_sections());
        let items = settings.items();
        settings.move_item(0, items.len());
        let chain = build_chain(&settings, None, 48_000).unwrap();
        let expected: Vec<u64> = items[1..].iter().chain(&items[..1]).map(|i| i.id).collect();
        assert_eq!(ids(&chain), expected);
    }

    #[test]
    fn every_kind_has_a_stage_and_unknown_ones_are_an_error() {
        assert_eq!(kinds().count(), default_sections().len());
        assert!(kinds().all(|k| new_section(k).is_some_and(|s| s.name == k)));
        assert!(new_section("Mystery").is_none());
        assert!(build_chain(&Settings::new(default_sections()), None, 48_000).is_ok());
        let odd = Settings::new(vec![crate::settings::Section::new("Mystery", vec![])]);
        let message = build_chain(&odd, None, 48_000).err().expect("unknown section").to_string();
        assert!(message.contains("Mystery"), "{message}");
    }

    #[test]
    fn the_eq_marks_the_first_enabled_de_esser_after_it() {
        let settings = Settings::new(["EQ", "De-esser", "EQ", "De-esser", "De-esser"].map(|k| new_section(k).unwrap()).into());
        let items = settings.items();
        let hz = |i: usize| deesser::frequency_hz(&items[i].section);
        items[3].section.enabled.store(false, Relaxed);
        items[4].section.params[0].set(items[4].section.params[0].max);
        assert_eq!(deesser_after(&items, items[0].id), Some(hz(1)));
        assert_eq!(deesser_after(&items, items[2].id), Some(hz(4)), "the bypassed one is skipped");
        assert_ne!(hz(1), hz(4));
        assert_eq!(deesser_after(&items, items[4].id), None, "nothing after the last");
    }

    /// Runs blocks until `done` says so, allowing a few seconds for a stage to build.
    fn run_until(live: &mut LiveChain, mut done: impl FnMut(&mut LiveChain, f32) -> bool) {
        let started = Instant::now();
        loop {
            let mut block = vec![0.1_f32; 480];
            live.process(&mut block);
            if done(live, block[479]) {
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "the chain never caught up");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn stage_addresses(live: &LiveChain) -> Vec<(u64, *const u8)> {
        live.chain.stages.iter().map(|(id, s)| (*id, &**s as *const dyn Processor as *const u8)).collect()
    }

    #[test]
    fn a_running_chain_follows_stages_being_added_moved_and_removed() {
        let settings = Settings::new(vec![gain::section()]);
        let mut live = LiveChain::new(settings.clone(), None, 48_000).unwrap();
        let first = stage_addresses(&live);

        let added = settings.add(gain::section());
        added.section.params[0].set(6.0);
        run_until(&mut live, |_, out| (out - 0.1 * 10f32.powf(6.0 / 20.0)).abs() < 1e-3);
        let two = stage_addresses(&live);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0], first[0], "the old stage was kept, not rebuilt");

        settings.move_item(1, 0);
        run_until(&mut live, |live, _| ids(&live.chain) == [added.id, first[0].0]);
        assert_eq!(stage_addresses(&live), [two[1], two[0]], "moving keeps both stages");

        let removed = settings.remove(added.id).unwrap();
        run_until(&mut live, |_, out| (out - 0.1).abs() < 1e-6);
        assert_eq!(stage_addresses(&live), first);

        settings.restore(removed);
        run_until(&mut live, |live, _| live.chain.stages.len() == 2);
    }

    #[test]
    fn a_slow_stage_is_built_without_holding_up_the_audio() {
        let settings = Settings::new(vec![gain::section()]);
        settings.items()[0].section.params[0].set(6.0);
        let mut live = LiveChain::new(settings.clone(), None, 48_000).unwrap();
        settings.add(deepfilter::section());

        let started = Instant::now();
        let mut block = vec![0.1_f32; 480];
        live.process(&mut block);
        assert!(started.elapsed() < Duration::from_millis(20), "took {:?}", started.elapsed());
        assert!((block[479] - 0.1 * 10f32.powf(6.0 / 20.0)).abs() < 1e-3, "the old chain kept running");
        run_until(&mut live, |live, _| live.chain.stages.len() == 2);
    }

    #[test]
    fn a_stage_that_will_not_build_is_reported_and_not_retried() {
        let settings = Settings::new(vec![gain::section()]);
        let mut live = LiveChain::new(settings.clone(), None, 48_000).unwrap();
        let odd = settings.add(crate::settings::Section::new("Mystery", vec![]));
        run_until(&mut live, |live, _| live.building.is_none() && live.seen == settings.generation());
        assert!(odd.section.status().contains("Mystery"));
        assert_eq!(live.failed, [odd.id]);
        assert_eq!(live.chain.stages.len(), 1, "the rest carries on");
    }

    #[test]
    fn full_chain_builds_and_passes_a_voice_band_tone() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings, None, 48_000).expect("chain builds");
        let mut tone = testutil::sine(500.0, 48_000, 0.05);
        chain.process(&mut tone);
        assert!(tone.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn full_chain_cost_is_a_small_fraction_of_real_time() {
        let settings = Settings::new(default_sections());
        let mut chain = build_chain(&settings, None, 48_000).expect("chain builds");
        let mut audio = testutil::sine(300.0, 48_000 * 5, 0.2);
        let started = std::time::Instant::now();
        for frame in audio.chunks_exact_mut(480) {
            chain.process(frame);
        }
        let factor = started.elapsed().as_secs_f32() / 5.0;
        eprintln!("full chain real-time factor: {factor:.3} (1.0 = one full core)");
        assert!(factor < 0.25, "chain too heavy: {factor}");
    }
}
