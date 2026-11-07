//! The Feedback suppressor's body: its knobs, a strip of the notches in use along a log
//! frequency axis, and a button that lets them all go.

use crate::dsp::feedback::{self, MAX_DEPTH};
use crate::knob::knob;
use crate::settings::Section;
use crate::theme;
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

const MIN_HZ: f32 = 100.0;
const MAX_HZ: f32 = 16_000.0;
const STRIP_HEIGHT: f32 = 64.0;
const MIN_WIDTH: f32 = 200.0;
/// Room under the markers for the axis labels.
const AXIS_HEIGHT: f32 = 16.0;
const GRID: [(f32, &str); 7] =
    [(100.0, "100"), (200.0, "200"), (500.0, "500"), (1_000.0, "1k"), (2_000.0, "2k"), (5_000.0, "5k"), (10_000.0, "10k")];
/// Short knob labels: longer ones overlap at the knob width; tooltips name them in full.
const KNOB_LABELS: [&str; 4] = ["Sense", "Notches", "Depth", "Release"];

/// Where things were drawn, so tests can aim the mouse.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Drawn {
    pub strip: Rect,
    pub clear: Rect,
}

/// 0 at `MIN_HZ`, 1 at `MAX_HZ`, logarithmic.
fn x_of(freq: f32) -> f32 {
    ((freq.max(MIN_HZ) / MIN_HZ).ln() / (MAX_HZ / MIN_HZ).ln()).clamp(0.0, 1.0)
}

fn freq_label(freq: f32) -> String {
    if freq >= 1_000.0 { format!("{:.2}k", freq / 1_000.0) } else { format!("{freq:.0}") }
}

pub fn ui(ui: &mut Ui, section: &Section, accent: Color32) -> Drawn {
    let color = if section.is_enabled() { accent } else { theme::OUTLINE_OFF };
    ui.add_enabled_ui(section.is_enabled(), |ui| {
        let strip = notch_strip(ui, section, color);
        ui.add_space(4.0);
        let clear = ui
            .horizontal_wrapped(|ui| {
                for (p, label) in section.params.iter().zip(KNOB_LABELS) {
                    knob(ui, p, label, color);
                }
                let clear = ui.button("Clear notches");
                if clear.clicked() {
                    feedback::clear(section);
                }
                theme::help_tooltip(
                    clear,
                    "Clear notches",
                    "Lets go of every cut at once. Use it after moving the mic or speakers; any howl that is still there is caught again.",
                )
                .rect
            })
            .inner;
        Drawn { strip, clear }
    })
    .inner
}

/// Each notch hangs from the top at its frequency: longer and brighter is deeper.
fn notch_strip(ui: &mut Ui, section: &Section, color: Color32) -> Rect {
    let width = ui.available_width().max(MIN_WIDTH);
    let (rect, response) = ui.allocate_exact_size(vec2(width, STRIP_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    let x = |freq: f32| rect.left() + x_of(freq) * rect.width();
    let small = FontId::proportional(10.0);
    painter.rect_filled(rect, 6.0, theme::PLOT_BG);
    for (freq, label) in GRID {
        painter.line_segment([pos2(x(freq), rect.top()), pos2(x(freq), rect.bottom())], Stroke::new(1.0, theme::GRID));
        painter.text(pos2(x(freq) + 2.0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, label, small.clone(), theme::TEXT_DIM);
    }

    let max_depth = section.get(MAX_DEPTH).max(1.0);
    let top = rect.top() + 4.0;
    let room = rect.height() - AXIS_HEIGHT - 8.0;
    let mut any = false;
    for (freq, depth) in feedback::notches(section) {
        any = true;
        let share = (depth / max_depth).clamp(0.0, 1.0);
        let shade = color.gamma_multiply(0.3 + 0.7 * share);
        let (px, end) = (x(freq), top + room * share.max(0.1));
        painter.line_segment([pos2(px, top), pos2(px, end)], Stroke::new(3.0, shade));
        painter.circle_filled(pos2(px, end), 3.0, shade);
        let (at, align) = if px > rect.right() - 48.0 { (px - 5.0, Align2::RIGHT_TOP) } else { (px + 5.0, Align2::LEFT_TOP) };
        painter.text(pos2(at, top), align, freq_label(freq), small.clone(), shade);
    }
    if !any {
        let middle = pos2(rect.center().x, rect.top() + (rect.height() - AXIS_HEIGHT) / 2.0);
        painter.text(middle, Align2::CENTER_CENTER, "No feedback heard", FontId::proportional(11.0), theme::TEXT_DIM);
    }
    theme::help_tooltip(
        response,
        "Notches",
        "Each mark is a cut on a howling note, low pitch on the left, high on the right. Longer and brighter is deeper. Marks shrink and fade as the cuts let go.",
    );
    rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2};

    fn painted_text(out: &egui::FullOutput) -> String {
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    fn frame(ctx: &egui::Context, section: &Section, width: f32, events: Vec<Event>) -> (Drawn, String) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 600.0))),
            events,
            ..Default::default()
        };
        let mut drawn = None;
        let out = ctx.run_ui(input, |ui| drawn = Some(super::ui(ui, section, Color32::LIGHT_BLUE)));
        (drawn.unwrap(), painted_text(&out))
    }

    #[test]
    fn each_notch_is_marked_with_its_frequency() {
        let section = feedback::section();
        let ctx = egui::Context::default();
        assert!(frame(&ctx, &section, 800.0, vec![]).1.contains("No feedback heard"));
        section.set_readout(0, 1_520.0);
        section.set_readout(1, 9.0);
        section.set_readout(4, 640.0);
        section.set_readout(5, 3.0);
        let text = frame(&ctx, &section, 800.0, vec![]).1;
        assert!(text.contains("1.52k") && text.contains("640"), "{text}");
        assert!(!text.contains("No feedback heard"));
    }

    #[test]
    fn clear_notches_empties_the_strip_and_asks_the_stage() {
        let section = feedback::section();
        section.set_readout(0, 2_000.0);
        section.set_readout(1, 12.0);
        let ctx = egui::Context::default();
        let at = frame(&ctx, &section, 800.0, vec![]).0.clear.center();
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(&ctx, &section, 800.0, vec![Event::PointerMoved(at)]);
        frame(&ctx, &section, 800.0, vec![press(true)]);
        frame(&ctx, &section, 800.0, vec![press(false)]);
        assert_eq!(feedback::notches(&section).count(), 0);
        assert!(section.take_clear_request());
    }

    #[test]
    fn fits_a_narrow_window() {
        let section = feedback::section();
        let ctx = egui::Context::default();
        let drawn = frame(&ctx, &section, 420.0, vec![]).0;
        assert!(drawn.strip.right() <= 420.0 && drawn.clear.right() <= 420.0, "{:?} {:?}", drawn.strip, drawn.clear);
    }

    #[test]
    fn the_axis_spans_100_hz_to_16_khz() {
        assert_eq!(x_of(MIN_HZ), 0.0);
        assert!((x_of(MAX_HZ) - 1.0).abs() < 1e-6);
        assert!(x_of(1_000.0) > 0.4 && x_of(1_000.0) < 0.5);
    }
}
