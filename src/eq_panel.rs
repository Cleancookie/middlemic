//! The EQ screen: response curve, draggable band nodes and a row of knobs per band.

use crate::dsp::{BANDS, response_db};
use crate::knob::knob;
use crate::theme;
use crate::settings::Section;
use crate::spectrum::{SAMPLE_RATE, freq_to_x, x_to_freq};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

/// The plot height is a share of the width, clamped (points).
const PLOT_HEIGHT: (f32, f32, f32) = (0.42, 140.0, 300.0);
const MIN_WIDTH: f32 = 200.0;
const CURVE_POINTS: usize = 200;
/// The EQ curve spans plus and minus this many dB.
const GAIN_RANGE_DB: f32 = 12.0;

const BAND_COLORS: [Color32; 5] = [
    Color32::from_rgb(190, 190, 200),
    Color32::from_rgb(240, 100, 100),
    Color32::from_rgb(245, 175, 70),
    Color32::from_rgb(90, 205, 130),
    Color32::from_rgb(100, 155, 245),
];
const GRID_FREQS: [(f32, &str); 10] = [
    (20.0, "20"),
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
    (20_000.0, "20k"),
];

/// Maps between screen positions and frequency and EQ gain.
struct Plot {
    rect: Rect,
}

impl Plot {
    fn x(&self, freq: f32) -> f32 {
        self.rect.left() + freq_to_x(freq) * self.rect.width()
    }

    fn freq(&self, x: f32) -> f32 {
        x_to_freq((x - self.rect.left()) / self.rect.width())
    }

    fn half_height(&self) -> f32 {
        self.rect.height() * 0.46
    }

    fn y_gain(&self, db: f32) -> f32 {
        self.rect.center().y - db / GAIN_RANGE_DB * self.half_height()
    }

    fn gain(&self, y: f32) -> f32 {
        (self.rect.center().y - y) / self.half_height() * GAIN_RANGE_DB
    }
}

pub struct EqPanel {
    /// Where the plot was drawn last frame (tests use it to aim the mouse).
    plot_rect: Rect,
}

impl Default for EqPanel {
    fn default() -> Self {
        Self { plot_rect: Rect::NOTHING }
    }
}

impl EqPanel {
    pub fn ui(&mut self, ui: &mut Ui, section: &Section) {
        ui.weak("drag the dots · double-click one to reset it");
        self.plot(ui, section);
        knob_row(ui, section);
    }

    fn plot(&mut self, ui: &mut Ui, section: &Section) {
        let (rect, _) = ui.allocate_exact_size(sized(ui, PLOT_HEIGHT), Sense::hover());
        self.plot_rect = rect;
        let plot = Plot { rect };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, theme::PLOT_BG);
        let on = section.is_enabled();

        for (freq, label) in GRID_FREQS {
            let x = plot.x(freq);
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.0, theme::GRID),
            );
            painter.text(
                pos2(x + 2.0, rect.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                label,
                FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }
        for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            let y = plot.y_gain(db);
            let line = if db == 0.0 { theme::GRID_STRONG } else { theme::GRID };
            painter.line_segment(
                [pos2(rect.left(), y), pos2(rect.right(), y)],
                Stroke::new(1.0, line),
            );
            painter.text(
                pos2(rect.left() + 3.0, y - 1.0),
                Align2::LEFT_BOTTOM,
                format!("{db:+.0}"),
                FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }

        // The EQ curve.
        let freqs: Vec<f32> = (0..CURVE_POINTS)
            .map(|i| x_to_freq(i as f32 / (CURVE_POINTS - 1) as f32))
            .collect();
        let curve: Vec<Pos2> = response_db(section, SAMPLE_RATE, &freqs)
            .iter()
            .zip(&freqs)
            .map(|(&db, &f)| pos2(plot.x(f), plot.y_gain(db.clamp(-GAIN_RANGE_DB * 1.5, GAIN_RANGE_DB * 1.5))))
            .collect();
        let curve_color = if on { Color32::WHITE } else { Color32::from_gray(90) };
        painter.add(Shape::line(curve, Stroke::new(2.0, curve_color)));

        // Band nodes. Interacting happens on the parent Ui, drawing on the clipped painter.
        for (i, band) in BANDS.iter().enumerate() {
            let freq = &section.params[band.freq];
            let gain = band.gain.map(|g| &section.params[g]);
            let centre = pos2(plot.x(freq.get()), plot.y_gain(gain.map_or(0.0, |g| g.get())));
            let response = ui.interact(
                Rect::from_center_size(centre, vec2(20.0, 20.0)),
                ui.make_persistent_id(("eq-node", band.label)),
                Sense::click_and_drag(),
            );
            if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.dragged()) {
                freq.set(plot.freq(pointer.x));
                if let Some(gain) = gain {
                    gain.set(plot.gain(pointer.y));
                }
            }
            if response.double_clicked() {
                freq.reset();
                gain.inspect(|g| g.reset());
            }
            let active = response.hovered() || response.dragged();
            let color = if on { BAND_COLORS[i] } else { Color32::from_gray(100) };
            painter.circle_filled(centre, if active { 9.0 } else { 7.0 }, color);
            painter.text(
                centre,
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                FontId::proportional(10.0),
                Color32::BLACK,
            );
            let now = match gain {
                Some(g) => format!("{} at {}", g.display(), freq.display()),
                None => freq.display(),
            };
            let hint = format!("Now {now}. Drag to move, double-click resets");
            theme::help_tooltip_with_hint(response, band.label, gain.map_or(freq.help, |g| g.help), &hint);
        }
    }
}

/// Full available width, with a height that follows it.
fn sized(ui: &Ui, (share, min, max): (f32, f32, f32)) -> egui::Vec2 {
    let width = ui.available_width().max(MIN_WIDTH);
    vec2(width, (width * share).clamp(min, max))
}

fn knob_row(ui: &mut Ui, section: &Section) {
    ui.horizontal_wrapped(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let margin = ui.spacing().window_margin.sum().x + 2.0;
        for (i, band) in BANDS.iter().enumerate() {
            // horizontal_wrapped cannot size a group before drawing it, so wrap by hand.
            let knobs = 1.0 + band.gain.is_some() as u8 as f32;
            let card = knobs * crate::knob::WIDTH + (knobs - 1.0) * gap + margin;
            if i > 0 && ui.available_width() < card {
                ui.end_row();
            }
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(format!("{} {}", i + 1, band.label)).color(BAND_COLORS[i]))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                    ui.horizontal(|ui| {
                        knob(ui, &section.params[band.freq], "Freq", BAND_COLORS[i]);
                        if let Some(gain) = band.gain {
                            knob(ui, &section.params[gain], "Gain", BAND_COLORS[i]);
                        }
                    });
                });
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_section() -> Section {
        crate::dsp::default_sections()
            .into_iter()
            .find(|s| s.name == crate::dsp::EQ_NAME)
            .unwrap()
    }

    fn draw(panel: &mut EqPanel, section: &Section) -> usize {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| panel.ui(ui, section)).shapes.len()
    }

    #[test]
    fn plot_maps_positions_both_ways() {
        let plot = Plot {
            rect: Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 200.0)),
        };
        for f in [20.0, 100.0, 1_000.0, 20_000.0] {
            assert!((plot.freq(plot.x(f)) - f).abs() / f < 1e-3);
        }
        for db in [-12.0, 0.0, 7.5, 12.0] {
            assert!((plot.gain(plot.y_gain(db)) - db).abs() < 1e-3);
        }
        assert!((plot.y_gain(0.0) - plot.rect.center().y).abs() < 1e-3);
        assert!(plot.y_gain(12.0) > plot.rect.top(), "+12 dB stays inside the box");
    }

    #[test]
    fn draws_the_whole_panel_headless() {
        let mut panel = EqPanel::default();
        assert!(draw(&mut panel, &eq_section()) > 50, "grid, curve, nodes and knobs");
    }

    /// One frame with the given mouse events.
    fn frame(ctx: &egui::Context, panel: &mut EqPanel, section: &Section, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 1000.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| panel.ui(ui, section));
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn dragging_a_node_changes_that_bands_frequency_and_gain() {
        let section = eq_section();
        let (ctx, mut panel) = (egui::Context::default(), EqPanel::default());
        frame(&ctx, &mut panel, &section, vec![]);
        let plot = Plot { rect: panel.plot_rect };

        let band = &BANDS[3]; // High-mid
        let (freq, gain) = (&section.params[band.freq], &section.params[band.gain.unwrap()]);
        let (freq0, gain0) = (freq.get(), gain.get());
        let start = pos2(plot.x(freq0), plot.y_gain(gain0));
        let end = start + vec2(90.0, -40.0);

        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(start)]);
        frame(&ctx, &mut panel, &section, vec![press(start, true)]);
        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(start + vec2(40.0, -20.0))]);
        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(end)]);
        frame(&ctx, &mut panel, &section, vec![press(end, false)]);

        assert!(freq.get() > freq0 * 1.2, "dragged right: {} -> {}", freq0, freq.get());
        assert!(gain.get() > gain0 + 2.0, "dragged up: {} -> {}", gain0, gain.get());
        for other in [1, 2, 4] {
            let b = &BANDS[other];
            assert_eq!(section.params[b.freq].get(), section.params[b.freq].default, "{}", b.label);
        }
    }

    #[test]
    fn dragging_the_high_pass_node_moves_only_its_frequency() {
        let section = eq_section();
        let (ctx, mut panel) = (egui::Context::default(), EqPanel::default());
        frame(&ctx, &mut panel, &section, vec![]);
        let plot = Plot { rect: panel.plot_rect };
        let hpf = &section.params[BANDS[0].freq];
        let start = pos2(plot.x(hpf.get()), plot.y_gain(0.0));

        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(start)]);
        frame(&ctx, &mut panel, &section, vec![press(start, true)]);
        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(start + vec2(30.0, 25.0))]);
        frame(&ctx, &mut panel, &section, vec![egui::Event::PointerMoved(start + vec2(60.0, 50.0))]);
        frame(&ctx, &mut panel, &section, vec![press(start + vec2(60.0, 50.0), false)]);

        assert!(hpf.get() > 100.0, "moved right: {}", hpf.get());
        let untouched = section.params.iter().filter(|p| p.unit == "dB");
        assert!(untouched.clone().count() > 0 && untouched.into_iter().all(|p| p.get() == 0.0));
    }

    #[test]
    fn drawing_never_changes_the_settings() {
        let section = eq_section();
        let before: Vec<f32> = section.params.iter().map(|p| p.get()).collect();
        draw(&mut EqPanel::default(), &section);
        let after: Vec<f32> = section.params.iter().map(|p| p.get()).collect();
        assert_eq!(before, after);
    }
}
