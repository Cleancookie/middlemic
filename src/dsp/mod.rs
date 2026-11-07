//! Platform-independent DSP. Every stage implements [`Processor`]; a [`Chain`]
//! runs them in order, so stages can be reordered, swapped or duplicated.

mod deepfilter;
mod gain;
mod limiter;

pub use deepfilter::DeepFilter;
pub use gain::Gain;
pub use limiter::Limiter;

/// One stage of the mic chain. Mono `f32` samples, processed in place.
/// Not `Send`: DeepFilterNet holds `Rc`. Build the chain on the thread that runs it.
pub trait Processor {
    fn name(&self) -> &str;

    /// Called once before processing, and again if the sample rate changes.
    fn prepare(&mut self, _sample_rate: u32) {}

    fn process(&mut self, samples: &mut [f32]);

    /// Extra delay this stage adds, in samples.
    fn latency_samples(&self) -> usize {
        0
    }
}

struct Slot {
    processor: Box<dyn Processor>,
    bypass: bool,
}

#[derive(Default)]
pub struct Chain {
    slots: Vec<Slot>,
}

impl Chain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, processor: impl Processor + 'static) -> &mut Self {
        self.slots.push(Slot {
            processor: Box::new(processor),
            bypass: false,
        });
        self
    }

    pub fn prepare(&mut self, sample_rate: u32) {
        for slot in &mut self.slots {
            slot.processor.prepare(sample_rate);
        }
    }

    pub fn set_bypass(&mut self, index: usize, bypass: bool) {
        if let Some(slot) = self.slots.get_mut(index) {
            slot.bypass = bypass;
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for slot in self.slots.iter_mut().filter(|s| !s.bypass) {
            slot.processor.process(samples);
        }
    }

    pub fn latency_samples(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| !s.bypass)
            .map(|s| s.processor.latency_samples())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_run_in_order() {
        let mut chain = Chain::new();
        chain.push(Gain::new(6.0)).push(Limiter::new(-6.0));
        chain.prepare(48_000);
        let mut buf = vec![0.5_f32; 480];
        chain.process(&mut buf);
        let ceiling = 10f32.powf(-6.0 / 20.0);
        assert!(buf.iter().all(|s| s.abs() <= ceiling + 1e-6));
    }

    #[test]
    fn bypassed_stage_is_skipped() {
        let mut chain = Chain::new();
        chain.push(Gain::new(20.0));
        chain.set_bypass(0, true);
        let mut buf = vec![0.1_f32; 8];
        chain.process(&mut buf);
        assert!(buf.iter().all(|s| (*s - 0.1).abs() < 1e-9));
    }

    #[test]
    fn same_stage_can_be_used_twice() {
        let mut chain = Chain::new();
        chain.push(Gain::new(6.0)).push(Gain::new(6.0));
        let mut buf = vec![0.1_f32; 4];
        chain.process(&mut buf);
        let expected = 0.1 * 10f32.powf(12.0 / 20.0);
        assert!((buf[0] - expected).abs() < 1e-5);
    }
}
