use super::Processor;

pub struct Gain {
    linear: f32,
}

impl Gain {
    pub fn new(db: f32) -> Self {
        Self {
            linear: 10f32.powf(db / 20.0),
        }
    }
}

impl Processor for Gain {
    fn name(&self) -> &str {
        "gain"
    }

    fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            *s *= self.linear;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_db_roughly_doubles() {
        let mut g = Gain::new(6.0206);
        let mut buf = [0.25_f32];
        g.process(&mut buf);
        assert!((buf[0] - 0.5).abs() < 1e-3);
    }
}
