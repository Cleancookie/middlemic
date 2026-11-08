//! Colours and global style: dark navy panels, thin coloured outlines, one accent per stage.
//! Every size is in egui points, so it follows the zoom factor and the display's DPI.

use crate::settings::Param;
use egui::{Color32, CornerRadius, Context, Response, RichText, Stroke, Ui, Visuals};

pub const BG: Color32 = Color32::from_rgb(13, 16, 23);
pub const PANEL: Color32 = Color32::from_rgb(22, 26, 36);
pub const PLOT_BG: Color32 = Color32::from_rgb(11, 14, 20);
pub const OUTLINE_OFF: Color32 = Color32::from_rgb(58, 63, 77);
pub const GRID: Color32 = Color32::from_rgb(38, 43, 56);
pub const GRID_STRONG: Color32 = Color32::from_rgb(70, 77, 96);
pub const TEXT: Color32 = Color32::from_rgb(230, 233, 242);
pub const TEXT_DIM: Color32 = Color32::from_rgb(138, 144, 162);
pub const WARN: Color32 = Color32::from_rgb(245, 170, 70);
pub const ERROR: Color32 = Color32::from_rgb(240, 112, 112);
pub const SPECTRUM_IN: Color32 = Color32::from_rgb(120, 128, 150);
pub const DEESSER: Color32 = Color32::from_rgb(61, 220, 224);

/// Violet, periwinkle, blue, cyan: the stage accents walk this ramp down the rack.
const RAMP: [Color32; 4] = [
    Color32::from_rgb(167, 139, 250),
    Color32::from_rgb(142, 162, 245),
    Color32::from_rgb(91, 156, 255),
    Color32::from_rgb(61, 220, 224),
];

/// Accent of row `index` of `count`, evenly spread along the ramp.
pub fn accent(index: usize, count: usize) -> Color32 {
    let t = if count > 1 { index.min(count - 1) as f32 / (count - 1) as f32 } else { 0.0 };
    let scaled = t * (RAMP.len() - 1) as f32;
    let i = (scaled as usize).min(RAMP.len() - 2);
    RAMP[i].lerp_to_gamma(RAMP[i + 1], scaled - i as f32)
}

/// Where a level bar turns amber and then red, as shares of the -60..0 dBFS bar.
const AMBER_FROM: f32 = 0.75;
const RED_FROM: f32 = 0.92;
const BAR_HEIGHT: f32 = 6.0;
const CLIP_CAP: f32 = 5.0;

/// A thin level meter with a visible track, so it reads as a meter even in silence.
pub fn level_bar(ui: &mut Ui, level: f32, width: f32) -> Response {
    level_bar_with_peak(ui, level, 0.0, false, width)
}

/// A level bar that turns amber then red near full scale, with a peak-hold `tick` above the
/// bar and a red cap at the end of the track while `clipped`. Click it to clear the clip.
pub fn level_bar_with_peak(ui: &mut Ui, level: f32, tick: f32, clipped: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, BAR_HEIGHT), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, GRID);
    let x_at = |share: f32| rect.left() + rect.width() * share.clamp(0.0, 1.0);
    let lit_to = x_at(level);
    for (from, to, color) in [(0.0, AMBER_FROM, accent(3, 4)), (AMBER_FROM, RED_FROM, WARN), (RED_FROM, 1.0, ERROR)] {
        let (x0, x1) = (x_at(from), x_at(to).min(lit_to));
        if x1 > x0 {
            let band = egui::Rect::from_x_y_ranges(x0..=x1, rect.y_range());
            painter.with_clip_rect(band).rect_filled(rect, 3.0, color);
        }
    }
    if tick > 0.02 {
        let x = x_at(tick).clamp(rect.left() + 1.0, rect.right() - 1.0);
        let color = if tick >= RED_FROM { ERROR } else if tick >= AMBER_FROM { WARN } else { TEXT };
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(1.5, color));
    }
    if clipped {
        let cap = egui::Rect::from_x_y_ranges(rect.right() - CLIP_CAP..=rect.right(), rect.y_range());
        painter.with_clip_rect(cap).rect_filled(rect, 3.0, ERROR);
    }
    response
}

const DIVIDER_HEIGHT: f32 = 14.0;

/// A 1 px vertical line centred on `centre`, in the subtle stroke colour.
pub fn paint_divider(painter: &egui::Painter, centre: egui::Pos2) {
    let half = egui::vec2(0.0, DIVIDER_HEIGHT / 2.0);
    painter.line_segment([centre - half, centre + half], Stroke::new(1.0, GRID_STRONG));
}

/// Separates groups of readouts in a row. Call `next` just before drawing each group that is
/// actually shown: the first gets no divider, so hidden groups never leave one dangling.
#[derive(Default)]
pub struct Dividers {
    shown_any: bool,
}

impl Dividers {
    pub fn next(&mut self, ui: &mut Ui) {
        if std::mem::replace(&mut self.shown_any, true) {
            divider(ui);
        }
    }
}

/// A divider as a layout item, for rows where the neighbours are always both shown.
pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, DIVIDER_HEIGHT), egui::Sense::hover());
    paint_divider(ui.painter(), rect.center());
}

/// 0..1 that eases in and out over 180 ms towards `on`. Repaints only while it moves.
pub fn animate(ctx: &Context, id: egui::Id, on: bool) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, 0.18, egui::emath::easing::cubic_in_out)
}

const TIP_WIDTH: f32 = 280.0;

/// Hover tooltip: `title` in full, `help` in plain words, and an optional dim `hint` on how to
/// operate the control. Shown on hover only, also while the control is disabled.
pub fn help_tooltip_with_hint(response: Response, title: &str, help: &str, hint: &str) -> Response {
    let show = |ui: &mut Ui| {
        ui.set_max_width(TIP_WIDTH);
        ui.label(RichText::new(title).strong().size(13.0).color(TEXT));
        if !help.is_empty() {
            ui.label(RichText::new(help).color(TEXT_DIM));
        }
        if !hint.is_empty() {
            ui.label(RichText::new(hint).size(10.5).color(TEXT_DIM));
        }
    };
    response.on_hover_ui(show).on_disabled_hover_ui(show)
}

pub fn help_tooltip(response: Response, title: &str, help: &str) -> Response {
    help_tooltip_with_hint(response, title, help, "")
}

/// Tooltip for a control that edits `param`: its full name and help text.
pub fn param_tooltip(response: Response, param: &Param, hint: &str) -> Response {
    help_tooltip_with_hint(response, param.name, param.help, hint)
}

pub fn apply(ctx: &Context) {
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, GRID);
    v.extreme_bg_color = PLOT_BG;
    v.faint_bg_color = PANEL;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    let blue = RAMP[2];
    v.selection.bg_fill = blue.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, blue);
    v.hyperlink_color = RAMP[3];

    let widgets = &mut v.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, GRID);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    for (w, fill) in [
        (&mut widgets.inactive, Color32::from_rgb(30, 35, 48)),
        (&mut widgets.hovered, Color32::from_rgb(38, 44, 60)),
        (&mut widgets.active, Color32::from_rgb(46, 53, 72)),
        (&mut widgets.open, Color32::from_rgb(38, 44, 60)),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(6);
    }
    widgets.inactive.bg_stroke = Stroke::new(1.0, GRID);
    widgets.hovered.bg_stroke = Stroke::new(1.0, blue);
    widgets.active.bg_stroke = Stroke::new(1.0, RAMP[3]);
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every piece of text painted in the frame, tooltips included.
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

    #[test]
    fn help_tooltip_shows_title_help_and_hint_only_on_hover() {
        let ctx = Context::default();
        ctx.global_style_mut(|s| {
            s.interaction.tooltip_delay = 0.0;
            s.interaction.show_tooltips_only_when_still = false;
        });
        let target = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(60.0, 20.0));
        let mut time = 0.0;
        let mut frame = |hover: Option<egui::Pos2>| {
            time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
                time: Some(time),
                events: hover.map(egui::Event::PointerMoved).into_iter().collect(),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                let response = ui.allocate_rect(target, egui::Sense::click());
                help_tooltip_with_hint(response, "Floor ratio", "Pushes quiet sound down.", "drag to change");
            })
        };
        let idle = painted_text(&frame(None));
        assert!(!idle.contains("Floor ratio"), "no tooltip without hover");
        let inside = target.center();
        let text = (0..4).map(|_| painted_text(&frame(Some(inside)))).collect::<Vec<_>>().join("|");
        for part in ["Floor ratio", "Pushes quiet sound down.", "drag to change"] {
            assert!(text.contains(part), "tooltip lacks {part:?}: {text}");
        }
    }

    #[test]
    fn dividers_appear_only_between_groups_that_are_shown() {
        let ctx = Context::default();
        let lines = |groups: &[bool]| {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.horizontal(|ui| {
                    let mut dividers = Dividers::default();
                    for _ in groups.iter().filter(|&&shown| shown) {
                        dividers.next(ui);
                        ui.label("x");
                    }
                });
            });
            out.shapes.iter().filter(|s| matches!(s.shape, egui::epaint::Shape::LineSegment { .. })).count()
        };
        assert_eq!(lines(&[]), 0);
        assert_eq!(lines(&[true]), 0, "no leading or trailing divider");
        assert_eq!(lines(&[true, false, true]), 1, "a hidden group leaves no doubled divider");
        assert_eq!(lines(&[true, true, true]), 2);
    }

    #[test]
    fn accents_run_from_violet_to_cyan() {
        assert_eq!(accent(0, 9), RAMP[0]);
        assert_eq!(accent(8, 9), RAMP[3]);
        assert_eq!(accent(0, 1), RAMP[0], "a lone row must not divide by zero");
        assert_eq!(accent(99, 3), RAMP[3], "out of range clamps");
    }
}
