//! Time-based smoothing for readouts, so values glide instead of jumping or flickering.
//! Driven by the frame's dt, so it looks the same at 20 fps and at 60.

use egui::{Context, Id};
use std::time::Duration;

/// Closer than this to the target counts as arrived.
const SETTLED: f32 = 0.01;
/// Matches the app's running repaint pace, so a glide never makes the UI busier than it already is.
const GLIDE_REPAINT: Duration = Duration::from_millis(50);

/// One step of exponential smoothing: `rise_tau` seconds when going up, `fall_tau` when going down.
pub fn ease_toward(current: f32, target: f32, dt: f32, rise_tau: f32, fall_tau: f32) -> f32 {
    let tau = if target > current { rise_tau } else { fall_tau };
    if tau <= 0.0 {
        return target;
    }
    let next = current + (target - current) * (1.0 - (-dt.max(0.0) / tau).exp());
    if (target - next).abs() < SETTLED { target } else { next }
}

/// A value kept in egui memory under `id` that glides to `target`. The first call snaps.
/// Asks for repaints only until it arrives.
pub fn glide(ctx: &Context, id: Id, target: f32, rise_tau: f32, fall_tau: f32) -> f32 {
    glide_from(ctx, id, target, target, rise_tau, fall_tau)
}

/// Like `glide`, but a value with no history starts at `start`, so it animates in.
/// Forget a finished one with `forget`.
pub fn glide_from(ctx: &Context, id: Id, start: f32, target: f32, rise_tau: f32, fall_tau: f32) -> f32 {
    let dt = ctx.input(|i| i.unstable_dt).min(0.1);
    let current = ctx.data_mut(|d| *d.get_temp_mut_or(id, start));
    let next = ease_toward(current, target, dt, rise_tau, fall_tau);
    ctx.data_mut(|d| d.insert_temp(id, next));
    if next != target {
        ctx.request_repaint_after(GLIDE_REPAINT);
    }
    next
}

pub fn forget(ctx: &Context, id: Id) {
    ctx.data_mut(|d| d.remove_temp::<f32>(id));
}

/// Gain reduction bites fast and lets go slowly, like the compressor it is showing.
pub fn glide_reduction(ctx: &Context, id: Id, db: f32) -> f32 {
    glide(ctx, id, db, 0.02, 0.25)
}

/// A 0..1 level meter reading: quick to rise, a short fall so 20 fps readings do not flicker.
pub fn glide_level(ctx: &Context, id: Id, norm: f32) -> f32 {
    glide(ctx, id, norm, 0.02, 0.18)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_is_faster_than_release() {
        let up = ease_toward(0.0, 10.0, 0.05, 0.03, 0.3);
        let down = ease_toward(10.0, 0.0, 0.05, 0.03, 0.3);
        assert!(up > 10.0 - down, "rose {up}, fell {}", 10.0 - down);
    }

    #[test]
    fn frame_rate_does_not_change_the_pace() {
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..10 {
            a = ease_toward(a, 1.0, 0.01, 0.1, 0.1);
        }
        b = ease_toward(b, 1.0, 0.1, 0.1, 0.1);
        assert!((a - b).abs() < 1e-4, "{a} vs {b}");
    }

    #[test]
    fn arrives_exactly_and_never_overshoots() {
        let mut v = 0.0;
        for _ in 0..200 {
            v = ease_toward(v, 1.0, 0.05, 0.05, 0.2);
            assert!(v <= 1.0);
        }
        assert_eq!(v, 1.0);
        assert_eq!(ease_toward(3.0, 0.0, 0.05, 0.0, 0.0), 0.0, "zero tau is instant");
    }

    #[test]
    fn glide_from_animates_in_and_forget_resets() {
        let ctx = Context::default();
        let id = Id::unique("from");
        let mut out = vec![];
        let _ = ctx.run_ui(egui::RawInput { predicted_dt: 0.05, ..Default::default() }, |_| {
            out.push(glide_from(&ctx, id, 0.0, 1.0, 0.1, 0.1));
            forget(&ctx, id);
            out.push(glide_from(&ctx, id, 0.0, 1.0, 0.1, 0.1));
        });
        assert!(out[0] > 0.0 && out[0] < 1.0);
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn glide_snaps_first_then_eases() {
        let ctx = Context::default();
        let id = Id::unique("g");
        let mut out = vec![];
        for target in [5.0, 0.0] {
            let _ = ctx.run_ui(egui::RawInput { predicted_dt: 0.05, ..Default::default() }, |_| {
                out.push(glide(&ctx, id, target, 0.0, 0.3));
            });
        }
        assert_eq!(out[0], 5.0);
        assert!(out[1] > 0.0 && out[1] < 5.0, "{}", out[1]);
    }
}
