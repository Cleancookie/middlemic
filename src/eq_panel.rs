//! The EQ screen: response curve over a live spectrum, draggable band nodes, a row of
//! knobs per band, and a scrolling spectrogram underneath.

use crate::dsp::{BANDS, response_db};
use crate::knob::knob;
use crate::theme;
use crate::settings::{Meters, Section};
use crate::spectrum::{
    Analyzer, FFT_LEN, FLOOR_DB, MAX_HZ, MIN_HZ, freq_to_x, heat, to_columns, x_to_freq,
};
use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions, Ui, pos2, vec2,
};
use std::time::{Duration, Instant};

const SAMPLE_RATE: f32 = 48_000.0;
/// Plot and spectrogram heights are a share of the width, clamped (points).
const PLOT_HEIGHT: (f32, f32, f32) = (0.42, 140.0, 300.0);
const SPECTROGRAM_HEIGHT: (f32, f32, f32) = (0.22, 80.0, 160.0);
const MIN_WIDTH: f32 = 200.0;
/// Columns across the plot.
const PLOT_POINTS: usize = 240;
const CURVE_POINTS: usize = 200;
/// Spectrogram size in pixels: about ten seconds of history.
const SG_WIDTH: usize = 220;
const SG_HEIGHT: usize = 120;
/// The analysis refreshes at about this interval, however often egui redraws.
const STEP: Duration = Duration::from_millis(45);
/// How fast the spectrum falls back after a peak.
const FALL_DB_PER_SECOND: f32 = 50.0;
/// The EQ curve spans plus and minus this many dB.
const GAIN_RANGE_DB: f32 = 12.0;
/// The spectrum is drawn between these levels.
const SPECTRUM_TOP_DB: f32 = -10.0;

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

/// Maps between screen positions and frequency, EQ gain and spectrum level.
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

    fn y_level(&self, db: f32) -> f32 {
        self.rect.bottom() - spectrum_height(db) * self.rect.height()
    }
}

/// 0 at the spectrum floor, 1 at its top.
fn spectrum_height(db: f32) -> f32 {
    ((db - FLOOR_DB) / (SPECTRUM_TOP_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

pub struct EqPanel {
    analyzer: Analyzer,
    samples: Vec<f32>,
    input_columns: Vec<f32>,
    output_columns: Vec<f32>,
    /// RGBA, `SG_WIDTH` by `SG_HEIGHT`, newest column on the right.
    spectrogram: Vec<u8>,
    texture: Option<TextureHandle>,
    texture_stale: bool,
    last_step: Instant,
    /// Where the plot was drawn last frame (tests use it to aim the mouse).
    plot_rect: Rect,
}

impl Default for EqPanel {
    fn default() -> Self {
        let mut spectrogram = vec![0u8; SG_WIDTH * SG_HEIGHT * 4];
        spectrogram.chunks_exact_mut(4).for_each(|px| px[3] = 255);
        Self {
            analyzer: Analyzer::default(),
            samples: vec![0.0; FFT_LEN],
            input_columns: vec![FLOOR_DB; PLOT_POINTS],
            output_columns: vec![FLOOR_DB; PLOT_POINTS],
            spectrogram,
            texture: None,
            texture_stale: true,
            last_step: Instant::now() - STEP * 2,
            plot_rect: Rect::NOTHING,
        }
    }
}

impl EqPanel {
    /// Pull the latest audio and refresh the displays. Cheap to call every frame: it does
    /// real work at most every `STEP`.
    pub fn update(&mut self, meters: &Meters, running: bool) {
        let elapsed = self.last_step.elapsed();
        if elapsed < STEP {
            return;
        }
        self.last_step = Instant::now();
        let fall = FALL_DB_PER_SECOND * elapsed.as_secs_f32().min(0.25);

        if !running {
            for v in self.input_columns.iter_mut().chain(&mut self.output_columns) {
                *v = (*v - fall).max(FLOOR_DB);
            }
            return;
        }

        meters.input_scope.read_latest(&mut self.samples);
        let bins = self.analyzer.analyze(&self.samples);
        smooth(&mut self.input_columns, &to_columns(&bins, SAMPLE_RATE, PLOT_POINTS), fall);

        meters.output_scope.read_latest(&mut self.samples);
        let bins = self.analyzer.analyze(&self.samples);
        smooth(&mut self.output_columns, &to_columns(&bins, SAMPLE_RATE, PLOT_POINTS), fall);
        self.push_spectrogram_column(&to_columns(&bins, SAMPLE_RATE, SG_HEIGHT));
    }

    /// The output spectrum as heights 0..1, in columns evenly spaced along `freq_to_x`.
    pub fn output_heights(&self) -> impl Iterator<Item = f32> + '_ {
        self.output_columns.iter().map(|&db| spectrum_height(db))
    }

    /// Scroll left by one pixel and draw `rows` (low to high frequency) as the new right edge.
    fn push_spectrogram_column(&mut self, rows: &[f32]) {
        for (y, row) in self.spectrogram.chunks_exact_mut(SG_WIDTH * 4).enumerate() {
            row.copy_within(4.., 0);
            let [r, g, b] = heat(rows[SG_HEIGHT - 1 - y]);
            row[(SG_WIDTH - 1) * 4..].copy_from_slice(&[r, g, b, 255]);
        }
        self.texture_stale = true;
    }

    /// `deesser_hz` marks where a de-esser after this EQ starts working, if there is one.
    pub fn ui(&mut self, ui: &mut Ui, section: &Section, deesser_hz: Option<f32>) {
        ui.weak("grey line: mic in · blue: after the whole chain · drag the dots");
        self.plot(ui, section);
        knob_row(ui, section);
        self.spectrogram_view(ui, deesser_hz);
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

        // Spectrum behind the curve: output filled, input as a line.
        let column_width = rect.width() / PLOT_POINTS as f32;
        for (i, &db) in self.output_columns.iter().enumerate() {
            let x = rect.left() + i as f32 * column_width;
            let top = plot.y_level(db);
            if top < rect.bottom() - 0.5 {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x, top), pos2(x + column_width + 0.5, rect.bottom())),
                    0.0,
                    Color32::from_rgba_unmultiplied(70, 130, 230, 85),
                );
            }
        }
        let input_line: Vec<Pos2> = self
            .input_columns
            .iter()
            .enumerate()
            .map(|(i, &db)| pos2(rect.left() + (i as f32 + 0.5) * column_width, plot.y_level(db)))
            .collect();
        painter.add(Shape::line(input_line, Stroke::new(1.0, theme::SPECTRUM_IN)));

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

    fn spectrogram_view(&mut self, ui: &mut Ui, deesser_hz: Option<f32>) {
        if self.texture_stale || self.texture.is_none() {
            let image = ColorImage::from_rgba_unmultiplied([SG_WIDTH, SG_HEIGHT], &self.spectrogram);
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ui.ctx().load_texture("spectrogram", image, TextureOptions::LINEAR));
                }
            }
            self.texture_stale = false;
        }
        let (rect, _) = ui.allocate_exact_size(sized(ui, SPECTROGRAM_HEIGHT), Sense::hover());
        let painter = ui.painter_at(rect);
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let y_of = |freq: f32| rect.bottom() - freq_to_x(freq) * rect.height();
        for (freq, label) in [(100.0, "100"), (500.0, "500"), (2_000.0, "2k"), (8_000.0, "8k")] {
            let y = y_of(freq);
            painter.line_segment(
                [pos2(rect.left(), y), pos2(rect.left() + 8.0, y)],
                Stroke::new(1.0, Color32::from_gray(150)),
            );
            painter.text(
                pos2(rect.left() + 11.0, y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(10.0),
                Color32::from_gray(190),
            );
        }
        if let Some(hz) = deesser_hz {
            let y = y_of(hz.clamp(MIN_HZ, MAX_HZ));
            painter.extend(Shape::dashed_line(
                &[pos2(rect.left() + 40.0, y), pos2(rect.right(), y)],
                Stroke::new(1.0, theme::DEESSER),
                5.0,
                4.0,
            ));
            painter.text(
                pos2(rect.right() - 4.0, y - 2.0),
                Align2::RIGHT_BOTTOM,
                "de-esser starts here",
                FontId::proportional(10.0),
                theme::DEESSER,
            );
        }
        painter.text(
            pos2(rect.right() - 4.0, rect.top() + 3.0),
            Align2::RIGHT_TOP,
            "spectrogram of the output, last ~10 s",
            FontId::proportional(10.0),
            Color32::from_gray(190),
        );
    }
}

/// Full available width, with a height that follows it.
fn sized(ui: &Ui, (share, min, max): (f32, f32, f32)) -> egui::Vec2 {
    let width = ui.available_width().max(MIN_WIDTH);
    vec2(width, (width * share).clamp(min, max))
}

/// Rises instantly, falls at a steady rate.
fn smooth(shown: &mut [f32], measured: &[f32], fall: f32) {
    for (v, &m) in shown.iter_mut().zip(measured) {
        *v = m.max(*v - fall);
    }
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
    use crate::dsp::testutil::sine;
    use crate::settings::{Scope, Settings};

    fn eq_section() -> Section {
        crate::dsp::default_sections()
            .into_iter()
            .find(|s| s.name == crate::dsp::EQ_NAME)
            .unwrap()
    }

    fn draw(panel: &mut EqPanel, section: &Section) -> usize {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| panel.ui(ui, section, Some(6_500.0)))
            .shapes
            .len()
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
        assert_eq!(plot.y_level(FLOOR_DB), plot.rect.bottom());
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
        let _ = ctx.run_ui(input, |ui| panel.ui(ui, section, Some(6_500.0)));
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

    #[test]
    fn audio_reaches_the_plot_and_the_spectrogram() {
        let settings = Settings::new(crate::dsp::default_sections());
        let mut panel = EqPanel::default();
        let tone = sine(1_000.0, Scope::LEN, 0.5);
        settings.meters.output_scope.write(&tone);
        settings.meters.input_scope.write(&tone);
        panel.update(&settings.meters, true);

        let loudest = panel
            .output_columns
            .iter()
            .enumerate()
            .fold((0, FLOOR_DB), |m, (i, &d)| if d > m.1 { (i, d) } else { m });
        let expected = (freq_to_x(1_000.0) * PLOT_POINTS as f32) as usize;
        assert!(loudest.0.abs_diff(expected) <= 2, "peak at column {}, wanted {expected}", loudest.0);
        assert!(loudest.1 > -12.0, "{}", loudest.1);

        let newest: Vec<u8> = (0..SG_HEIGHT)
            .flat_map(|y| panel.spectrogram[(y * SG_WIDTH + SG_WIDTH - 1) * 4..][..3].to_vec())
            .collect();
        assert!(newest.iter().any(|&c| c > 100), "the new spectrogram column is lit");
        assert!(panel.texture_stale);
    }

    #[test]
    fn spectrogram_scrolls_left() {
        let mut panel = EqPanel::default();
        panel.push_spectrogram_column(&vec![-20.0; SG_HEIGHT]);
        panel.push_spectrogram_column(&vec![FLOOR_DB; SG_HEIGHT]);
        let px = |x: usize| panel.spectrogram[(x) * 4..][..3].to_vec();
        assert_ne!(px(SG_WIDTH - 2), vec![0, 0, 0], "the loud column moved one pixel left");
        assert_eq!(px(SG_WIDTH - 1), vec![0, 0, 0]);
    }

    #[test]
    fn display_falls_back_slowly_after_the_sound_stops() {
        let mut shown = vec![-20.0];
        smooth(&mut shown, &[FLOOR_DB], 5.0);
        assert_eq!(shown, vec![-25.0]);
        smooth(&mut shown, &[-10.0], 5.0);
        assert_eq!(shown, vec![-10.0], "rises instantly");
    }

    #[test]
    fn stopped_engine_fades_the_display_instead_of_freezing_it() {
        let settings = Settings::new(crate::dsp::default_sections());
        let mut panel = EqPanel::default();
        panel.output_columns.iter_mut().for_each(|v| *v = -20.0);
        panel.update(&settings.meters, false);
        assert!(panel.output_columns[0] < -20.0);
    }
}
