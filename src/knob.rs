//! A flat rotary knob: a ring with a value arc in the stage accent. Drag up/down to turn, shift for fine control,
//! scroll to nudge, double-click to reset.

use crate::settings::Param;
use crate::theme;
use egui::{Color32, Pos2, Sense, Shape, Stroke, Ui, vec2};
use std::f32::consts::PI;

const SIZE: f32 = 46.0;
/// Width a knob takes in a row, for callers that lay out by hand.
pub const WIDTH: f32 = SIZE + 12.0;
const RADIUS: f32 = 17.0;
/// The dial sweeps 270 degrees, from bottom-left round the top to bottom-right.
const START: f32 = 0.75 * PI;
const SWEEP: f32 = 1.5 * PI;
const DRAG_PIXELS: f32 = 160.0;
const FINE_DRAG_PIXELS: f32 = 800.0;
const SCROLL_PIXELS: f32 = 1500.0;
const HINT: &str = "Drag, scroll, shift for fine, double-click resets, right-click for more";

fn angle(norm: f32) -> f32 {
    START + SWEEP * norm
}

fn arc(centre: Pos2, from: f32, to: f32) -> Vec<Pos2> {
    let steps = (((to - from).abs() * 24.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = angle(from + (to - from) * i as f32 / steps as f32);
            centre + vec2(a.cos(), a.sin()) * RADIUS
        })
        .collect()
}

/// New position after the user's input, or `None` if nothing changed.
/// `limited` keeps it on the dial; otherwise it may run past either end.
fn turned(norm: f32, drag_up: f32, scroll_up: f32, fine: bool, limited: bool) -> Option<f32> {
    let per_pixel = 1.0 / if fine { FINE_DRAG_PIXELS } else { DRAG_PIXELS };
    let delta = drag_up * per_pixel + scroll_up / SCROLL_PIXELS;
    (delta != 0.0).then(|| if limited { (norm + delta).clamp(0.0, 1.0) } else { norm + delta })
}

/// Space under a knob's label, between grid rows.
const ROW_GAP: f32 = 8.0;

fn label_width(ui: &Ui, label: &str) -> f32 {
    let text = label.to_uppercase();
    let galley = ui.painter().layout_no_wrap(text.clone(), egui::FontId::proportional(9.5), theme::TEXT_DIM);
    galley.size().x + 0.8 * text.chars().count() as f32 + 4.0
}

/// A dialog for typing an exact value. Enter applies it, Escape or a click outside cancels.
fn edit_modal(ui: &Ui, id: egui::Id, param: &Param, label: &str) {
    let Some(mut text) = ui.data(|d| d.get_temp::<String>(id)) else { return };
    let mut done = false;
    let modal = egui::Modal::new(id).show(ui.ctx(), |ui| {
        ui.set_width(220.0);
        ui.heading(label);
        let unit = if param.unit.is_empty() { String::new() } else { format!(" ({})", param.unit) };
        ui.label(format!("Value{unit}"));
        let field = ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
        field.request_focus();
        if !param.is_unlimited() {
            ui.label(egui::RichText::new(format!("{} to {}", param.min, param.max)).size(10.5).color(theme::TEXT_DIM));
        }
        let valid = text.trim().parse::<f32>().ok().filter(|v| v.is_finite());
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            if (ui.add_enabled(valid.is_some(), egui::Button::new("OK")).clicked() || enter) && valid.is_some() {
                param.set(valid.unwrap_or_default());
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
    });
    if done || modal.should_close() {
        ui.data_mut(|d| d.remove::<String>(id));
    } else {
        ui.data_mut(|d| d.insert_temp(id, text));
    }
}

/// Draws the knob with `label` underneath; the label shows the value while hovered or dragged.
pub fn knob(ui: &mut Ui, param: &Param, label: &str, color: Color32) -> egui::Response {
    // Long names such as "MAX REDUCTION" widen the cell rather than spill over the neighbours.
    let width = WIDTH.max(label_width(ui, label));
    ui.vertical(|ui| {
        ui.set_width(width);
        // Label hugs its dial; the gap to the next row's label comes after the value instead.
        ui.spacing_mut().item_spacing.y = 0.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, SIZE), Sense::click_and_drag());
        let centre = rect.center();

        let fine = ui.input(|i| i.modifiers.shift);
        let scroll = if response.hovered() {
            ui.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        if let Some(n) = turned(param.to_norm(param.get()), -response.drag_delta().y, scroll, fine, !param.is_unlimited()) {
            param.set(param.from_norm(n));
        }
        if response.double_clicked() {
            param.reset();
        }

        let edit_id = response.id.with("edit");
        response.context_menu(|ui| {
            if ui.button("Edit value…").clicked() {
                ui.data_mut(|d| d.insert_temp(edit_id, format!("{}", param.get())));
                ui.close();
            }
            let mut unlimited = param.is_unlimited();
            if ui.checkbox(&mut unlimited, "Remove min/max limit").changed() {
                param.set_unlimited(unlimited);
            }
            ui.label(egui::RichText::new("Values outside the normal range are unsupported.").size(10.5).color(theme::TEXT_DIM));
        });

        edit_modal(ui, edit_id, param, label);

        let overshoot = param.is_unlimited() && !(param.min..=param.max).contains(&param.get());
        let norm = param.to_norm(param.get()).clamp(0.0, 1.0);
        // Knobs with a negative side fill from the middle, like a pan knob.
        let origin = if param.min < 0.0 && param.max > 0.0 {
            param.to_norm(0.0)
        } else {
            0.0
        };
        let hover = theme::animate(ui.ctx(), response.id.with("hover"), response.hovered() || response.dragged());
        let painter = ui.painter();
        painter.add(Shape::line(arc(centre, 0.0, 1.0), Stroke::new(2.5, theme::GRID_STRONG)));
        painter.circle_filled(centre, RADIUS - 5.0, theme::PANEL.lerp_to_gamma(theme::GRID, hover));
        if (norm - origin).abs() > 0.002 {
            painter.add(Shape::line(arc(centre, origin, norm), Stroke::new(3.0, if overshoot { theme::WARN } else { color })));
        }
        let a = angle(norm);
        let dir = vec2(a.cos(), a.sin());
        painter.line_segment(
            [centre + dir * (RADIUS * 0.15), centre + dir * (RADIUS * 0.6)],
            Stroke::new(2.0, if overshoot { theme::WARN } else { theme::TEXT }),
        );

        // Like Vital: only the label shows, and it gives way to the value while you point at or turn the knob.
        let caption = ui
            .vertical_centered(|ui| {
                let text = if response.hovered() || response.dragged() {
                    egui::RichText::new(param.display()).size(10.5).color(theme::TEXT)
                } else {
                    egui::RichText::new(label.to_uppercase()).size(9.5).extra_letter_spacing(0.8).color(theme::TEXT_DIM)
                };
                ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend).sense(Sense::hover()))
            })
            .inner;
        theme::param_tooltip(caption, param, HINT);
        ui.add_space(ROW_GAP);
        theme::param_tooltip(response, param, HINT)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_up_raises_and_clamps() {
        assert!(turned(0.5, 16.0, 0.0, false, true).unwrap() > 0.5);
        assert!(turned(0.5, -16.0, 0.0, false, true).unwrap() < 0.5);
        assert_eq!(turned(0.99, 500.0, 0.0, false, true), Some(1.0));
        assert_eq!(turned(0.01, -500.0, 0.0, false, true), Some(0.0));
    }

    #[test]
    fn without_a_limit_it_runs_past_the_ends() {
        assert!(turned(0.99, 500.0, 0.0, false, false).unwrap() > 1.0);
        assert!(turned(0.01, -500.0, 0.0, false, false).unwrap() < 0.0);
    }

    #[test]
    fn shift_makes_the_same_drag_finer() {
        let coarse = turned(0.5, 20.0, 0.0, false, true).unwrap() - 0.5;
        let fine = turned(0.5, 20.0, 0.0, true, true).unwrap() - 0.5;
        assert!(fine > 0.0 && fine < coarse / 3.0);
    }

    #[test]
    fn no_input_means_no_change() {
        assert_eq!(turned(0.5, 0.0, 0.0, false, true), None);
    }

    #[test]
    fn scroll_nudges() {
        assert!(turned(0.5, 0.0, 120.0, false, true).unwrap() > 0.5);
    }

    #[test]
    fn the_dial_sweeps_270_degrees_with_a_gap_at_the_bottom() {
        assert!((angle(1.0) - angle(0.0) - 1.5 * PI).abs() < 1e-6);
        let mid = angle(0.5);
        assert!((mid.sin() + 1.0).abs() < 1e-6, "halfway points straight up");
    }

    #[test]
    fn a_real_mouse_drag_turns_the_knob() {
        let param = Param::new("Gain", "dB", -12.0, 12.0, 0.0);
        let ctx = egui::Context::default();
        let rect = std::cell::Cell::new(egui::Rect::NOTHING);
        let frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| rect.set(knob(ui, &param, "Gain", Color32::RED).rect));
        };
        frame(vec![]);
        let start = rect.get().left_top() + vec2(SIZE / 2.0 + 6.0, SIZE / 2.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(start)]);
        frame(vec![button(start, true)]);
        frame(vec![egui::Event::PointerMoved(start - vec2(0.0, 20.0))]);
        frame(vec![egui::Event::PointerMoved(start - vec2(0.0, 40.0))]);
        frame(vec![button(start - vec2(0.0, 40.0), false)]);
        assert!(param.get() > 3.0, "dragging up should raise it, got {}", param.get());
    }

    #[test]
    fn draws_without_panicking_and_reflects_the_value() {
        let param = Param::new("Gain", "dB", -12.0, 12.0, 6.0);
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            knob(ui, &param, "Gain", Color32::RED);
        });
        assert!(!out.shapes.is_empty());
        assert_eq!(param.get(), 6.0, "drawing alone must not change the value");
    }
}
