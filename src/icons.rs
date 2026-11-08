//! Painted icon buttons: egui's default fonts lack glyphs such as ✕, ⟳ and ⚙.

use egui::{Pos2, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, vec2};
use std::f32::consts::{FRAC_PI_2, TAU};

/// A framed square button; `draw` paints the icon given its centre, radius and stroke.
fn icon_button(ui: &mut Ui, side: f32, radius: f32, draw: impl FnOnce(&egui::Painter, Pos2, f32, Stroke)) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        painter.rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, StrokeKind::Inside);
        draw(painter, rect.center(), radius, Stroke::new(1.5, visuals.fg_stroke.color));
    }
    response
}

pub fn cross_button(ui: &mut Ui) -> Response {
    icon_button(ui, 18.0, 3.5, |painter, c, r, stroke| {
        painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
        painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
    })
}

/// A circular arrow.
pub fn refresh_button(ui: &mut Ui) -> Response {
    let side = ui.spacing().interact_size.y;
    icon_button(ui, side, side * 0.28, |painter, c, r, stroke| {
        let at = |a: f32| c + r * Vec2::angled(a);
        // A 300° arc with a gap at the top right, arrowhead on its clockwise end.
        let (start, end) = (-1.2_f32, 4.0_f32);
        let points: Vec<_> = (0..=24).map(|i| at(start + (end - start) * i as f32 / 24.0)).collect();
        painter.add(Shape::line(points, stroke));
        let tip = at(start);
        let tangent = Vec2::angled(start - FRAC_PI_2);
        let side = Vec2::angled(start);
        let head = vec![tip, tip - tangent * 3.5 + side * 3.0, tip - tangent * 3.5 - side * 3.0];
        painter.add(Shape::convex_polygon(head, stroke.color, Stroke::NONE));
    })
}

/// A cog: eight teeth round a hub.
pub fn gear_button(ui: &mut Ui) -> Response {
    const TEETH: usize = 8;
    // Each tooth: (fraction of a tooth's arc, radius), root then tip then root again.
    const TOOTH: [(f32, f32); 4] = [(0.05, 0.75), (0.2, 1.15), (0.45, 1.15), (0.6, 0.75)];
    let side = ui.spacing().interact_size.y;
    icon_button(ui, side, side * 0.3, |painter, c, r, stroke| {
        let outline: Vec<_> = (0..TEETH)
            .flat_map(|tooth| {
                TOOTH.map(|(along, radius)| c + r * radius * Vec2::angled((tooth as f32 + along) * TAU / TEETH as f32))
            })
            .collect();
        painter.add(Shape::closed_line(outline, stroke));
        painter.circle_stroke(c, r * 0.3, stroke);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_buttons_are_square_and_clickable() {
        let ctx = egui::Context::default();
        let mut sizes = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            for button in [cross_button, refresh_button, gear_button] {
                let r = button(ui);
                assert!(r.sense.senses_click());
                sizes.push(r.rect.size());
            }
        });
        assert!(sizes.iter().all(|s| s.x == s.y && s.x >= 18.0), "{sizes:?}");
    }
}
