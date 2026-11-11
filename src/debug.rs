//! The global debug switch. While it is off the engine skips its timing and the UI hides the
//! CPU and per-stage load readouts. A static, because the audio thread reads it every block.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ON: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ON.load(Relaxed)
}

pub fn set(on: bool) {
    ON.store(on, Relaxed);
}
