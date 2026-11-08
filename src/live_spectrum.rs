//! The live analyser behind the bottom pane: the mic and the output as a spectrum, and a
//! scrolling spectrogram of the output. One instance feeds every display, so the FFTs run once.

use crate::latency::load_text;
use crate::settings::Meters;
use crate::spectrum::{
    Analyzer, FFT_LEN, FLOOR_DB, MAX_HZ, MIN_HZ, SAMPLE_RATE, freq_to_x, heat, to_columns,
};
use crate::theme;
use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions, Ui, pos2, vec2,
};
use std::time::{Duration, Instant};

/// Plot and spectrogram heights are a share of their width, clamped (points).
const HEIGHT: (f32, f32, f32) = (0.32, 90.0, 150.0);
const MIN_WIDTH: f32 = 160.0;
const HEADER_HEIGHT: f32 = 22.0;
/// Columns across the plot.
const PLOT_POINTS: usize = 240;
/// Spectrogram size in pixels: about ten seconds of history.
const SG_WIDTH: usize = 220;
const SG_HEIGHT: usize = 120;
/// The analysis refreshes at about this interval, however often egui redraws.
const STEP: Duration = Duration::from_millis(45);
/// How fast the spectrum falls back after a peak.
const FALL_DB_PER_SECOND: f32 = 50.0;
/// The spectrum is drawn between these levels.
const SPECTRUM_TOP_DB: f32 = -10.0;
const GRID_FREQS: [(f32, &str); 7] = [
    (50.0, "50"),
    (100.0, "100"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
];

/// The views of the bottom pane.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Tab {
    #[default]
    Spectrum,
    Takes,
}

impl Tab {
    const ALL: [Tab; 2] = [Tab::Spectrum, Tab::Takes];

    fn title(self) -> &'static str {
        match self {
            Tab::Spectrum => "SPECTRUM",
            Tab::Takes => "TAKES",
        }
    }

    fn tip_title(self) -> &'static str {
        match self {
            Tab::Spectrum => "Spectrum",
            Tab::Takes => "Takes",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tab::Spectrum => "Live frequency view of your mic and of the sound after the whole rack, whichever stage is open.",
            Tab::Takes => "Record your raw mic and play it back through the rack to hear every knob you move.",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tab::Spectrum => "spectrum",
            Tab::Takes => "takes",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL.into_iter().find(|t| t.key() == key).unwrap_or_default()
    }

    /// Whether the analyser has anything to show: the spectrum tab when unfolded, or an open
    /// compressor row, which draws the spectrum behind its curve.
    pub fn needs_analysis(open: bool, tab: Tab, compressor_open: bool) -> bool {
        (open && tab == Tab::Spectrum) || compressor_open
    }
}

/// 0 at the spectrum floor, 1 at its top.
fn spectrum_height(db: f32) -> f32 {
    ((db - FLOOR_DB) / (SPECTRUM_TOP_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

pub struct LiveSpectrum {
    analyzer: Analyzer,
    samples: Vec<f32>,
    input_columns: Vec<f32>,
    output_columns: Vec<f32>,
    /// RGBA, `SG_WIDTH` by `SG_HEIGHT`, newest column on the right.
    spectrogram: Vec<u8>,
    texture: Option<TextureHandle>,
    texture_stale: bool,
    last_step: Instant,
}

impl Default for LiveSpectrum {
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
        }
    }
}

impl LiveSpectrum {
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

    /// A header with a tab per view that folds the pane when clicked elsewhere, then the
    /// selected view when `open`. `takes` draws the Takes tab.
    pub fn pane(
        &mut self,
        ui: &mut Ui,
        open: &mut bool,
        tab: &mut Tab,
        deesser_hz: Option<f32>,
        load: Option<f32>,
        takes: impl FnOnce(&mut Ui),
    ) {
        let (rect, header) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::click());
        let color = if header.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        let c = pos2(rect.left() + 8.0, rect.center().y);
        let tip = if *open { [vec2(-4.0, -2.5), vec2(4.0, -2.5), vec2(0.0, 3.0)] } else { [vec2(-2.5, -4.0), vec2(-2.5, 4.0), vec2(3.0, 0.0)] };
        ui.painter().add(Shape::convex_polygon(tip.iter().map(|&v| c + v).collect(), color, Stroke::NONE));
        let mut x = rect.left() + 14.0;
        for t in Tab::ALL {
            let galley = ui.painter().layout_no_wrap(t.title().into(), FontId::proportional(12.0), Color32::PLACEHOLDER);
            let slot = Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x + 12.0, rect.height()));
            let response = ui.interact(slot, ui.make_persistent_id(("pane-tab", t as u8)), Sense::click());
            let selected = *open && *tab == t;
            let text = if selected || response.hovered() { theme::TEXT } else { theme::TEXT_DIM };
            let at = pos2(slot.left() + 6.0, rect.center().y - galley.size().y / 2.0);
            if selected {
                let y = rect.bottom() - 1.0;
                ui.painter().line_segment([pos2(slot.left() + 6.0, y), pos2(slot.right() - 6.0, y)], Stroke::new(2.0, theme::accent(2, 4)));
            }
            ui.painter().galley(at, galley, text);
            if response.clicked() {
                *tab = t;
                *open = true;
            }
            theme::help_tooltip(response.on_hover_cursor(egui::CursorIcon::PointingHand), t.tip_title(), t.tip());
            x = slot.right();
        }
        if let Some(load) = load {
            let text_rect = ui.painter().text(
                pos2(rect.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                format!("all stages {}", load_text(load)),
                FontId::monospace(11.0),
                theme::TEXT_DIM,
            );
            theme::paint_divider(ui.painter(), pos2(text_rect.left() - 8.0, rect.center().y));
        }
        if header.clicked() {
            *open = !*open;
        }
        theme::help_tooltip_with_hint(
            header.on_hover_cursor(egui::CursorIcon::PointingHand),
            "Bottom pane",
            "The figure on the right is the processing load of all stages together, as a share of one CPU core.",
            "Click to fold or unfold",
        );
        if *open {
            match *tab {
                Tab::Spectrum => self.ui(ui, deesser_hz),
                Tab::Takes => takes(ui),
            }
        }
    }

    /// The spectrum and the spectrogram side by side. `deesser_hz` marks where an enabled
    /// de-esser starts working on the spectrogram, if there is one.
    pub fn ui(&mut self, ui: &mut Ui, deesser_hz: Option<f32>) {
        let width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(MIN_WIDTH);
        let size = vec2(width, (width * HEIGHT.0).clamp(HEIGHT.1, HEIGHT.2));
        ui.horizontal_top(|ui| {
            self.plot(ui, size);
            self.spectrogram_view(ui, size, deesser_hz);
        });
    }

    fn plot(&self, ui: &mut Ui, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        theme::help_tooltip(
            response,
            "Spectrum",
            "Live frequency view. The grey line is your mic before the rack; the blue fill is the sound after every stage.",
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, theme::PLOT_BG);
        let x_of = |freq: f32| rect.left() + freq_to_x(freq) * rect.width();
        for (freq, label) in GRID_FREQS {
            let x = x_of(freq);
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, theme::GRID));
            painter.text(
                pos2(x + 2.0, rect.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                label,
                FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }
        let y_of = |db: f32| rect.bottom() - spectrum_height(db) * rect.height();
        let column_width = rect.width() / PLOT_POINTS as f32;
        for (i, &db) in self.output_columns.iter().enumerate() {
            let x = rect.left() + i as f32 * column_width;
            let top = y_of(db);
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
            .map(|(i, &db)| pos2(rect.left() + (i as f32 + 0.5) * column_width, y_of(db)))
            .collect();
        painter.add(Shape::line(input_line, Stroke::new(1.0, theme::SPECTRUM_IN)));
    }

    fn spectrogram_view(&mut self, ui: &mut Ui, size: egui::Vec2, deesser_hz: Option<f32>) {
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
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        theme::help_tooltip(
            response,
            "Spectrogram",
            "The sound after the rack, scrolling left over the last ten seconds. Pitch runs up the side and brighter means louder.",
        );
        let painter = ui.painter_at(rect);
        if let Some(texture) = &self.texture {
            painter.image(texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
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
    }
}

/// Rises instantly, falls at a steady rate.
fn smooth(shown: &mut [f32], measured: &[f32], fall: f32) {
    for (v, &m) in shown.iter_mut().zip(measured) {
        *v = m.max(*v - fall);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testutil::sine;
    use crate::settings::{Scope, Settings};

    #[test]
    fn draws_both_views_headless() {
        let mut spectrum = LiveSpectrum::default();
        let ctx = egui::Context::default();
        let shapes = ctx.run_ui(egui::RawInput::default(), |ui| spectrum.ui(ui, Some(6_500.0))).shapes.len();
        assert!(shapes > 15, "grid, curves and spectrogram: {shapes}");
    }

    #[test]
    fn the_header_folds_the_pane_and_the_displays_follow() {
        let (ctx, mut spectrum) = (egui::Context::default(), LiveSpectrum::default());
        let shapes = |open: &mut bool, spectrum: &mut LiveSpectrum| {
            let mut tab = Tab::Spectrum;
            ctx.run_ui(egui::RawInput::default(), |ui| spectrum.pane(ui, open, &mut tab, None, Some(0.05), |_| {})).shapes.len()
        };
        let (mut open, mut closed) = (true, false);
        assert!(shapes(&mut open, &mut spectrum) > shapes(&mut closed, &mut spectrum) + 10);
        assert!(!closed, "drawing alone never toggles it");
    }

    #[test]
    fn the_takes_tab_draws_the_callers_content_instead_of_the_spectrum() {
        let (ctx, mut spectrum) = (egui::Context::default(), LiveSpectrum::default());
        let (mut open, mut tab, mut drawn) = (true, Tab::Takes, false);
        let shapes = ctx
            .run_ui(egui::RawInput::default(), |ui| spectrum.pane(ui, &mut open, &mut tab, None, None, |_| drawn = true))
            .shapes
            .len();
        assert!(drawn && shapes < 15, "{shapes} shapes");
    }

    #[test]
    fn tabs_round_trip_through_settings_and_unknown_falls_back() {
        for t in Tab::ALL {
            assert_eq!(Tab::from_key(t.key()), t);
        }
        assert_eq!(Tab::from_key("nonsense"), Tab::Spectrum);
    }

    #[test]
    fn the_analyser_is_skipped_unless_something_shows_it() {
        assert!(Tab::needs_analysis(true, Tab::Spectrum, false));
        assert!(!Tab::needs_analysis(false, Tab::Spectrum, false), "folded");
        assert!(!Tab::needs_analysis(true, Tab::Takes, false), "takes tab");
        assert!(Tab::needs_analysis(true, Tab::Takes, true), "compressor row open");
        assert!(Tab::needs_analysis(false, Tab::Takes, true));
    }

    #[test]
    fn audio_reaches_the_plot_and_the_spectrogram() {
        let settings = Settings::new(crate::dsp::default_sections());
        let mut spectrum = LiveSpectrum::default();
        let tone = sine(1_000.0, Scope::LEN, 0.5);
        settings.meters.output_scope.write(&tone);
        settings.meters.input_scope.write(&tone);
        spectrum.update(&settings.meters, true);

        let loudest = spectrum
            .output_columns
            .iter()
            .enumerate()
            .fold((0, FLOOR_DB), |m, (i, &d)| if d > m.1 { (i, d) } else { m });
        let expected = (freq_to_x(1_000.0) * PLOT_POINTS as f32) as usize;
        assert!(loudest.0.abs_diff(expected) <= 2, "peak at column {}, wanted {expected}", loudest.0);
        assert!(loudest.1 > -12.0, "{}", loudest.1);

        let newest: Vec<u8> = (0..SG_HEIGHT)
            .flat_map(|y| spectrum.spectrogram[(y * SG_WIDTH + SG_WIDTH - 1) * 4..][..3].to_vec())
            .collect();
        assert!(newest.iter().any(|&c| c > 100), "the new spectrogram column is lit");
        assert!(spectrum.texture_stale);
        assert!(spectrum.output_heights().any(|h| h > 0.5), "the compressor panel sees it too");
    }

    #[test]
    fn spectrogram_scrolls_left() {
        let mut spectrum = LiveSpectrum::default();
        spectrum.push_spectrogram_column(&vec![-20.0; SG_HEIGHT]);
        spectrum.push_spectrogram_column(&vec![FLOOR_DB; SG_HEIGHT]);
        let px = |x: usize| spectrum.spectrogram[(x) * 4..][..3].to_vec();
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
        let mut spectrum = LiveSpectrum::default();
        spectrum.output_columns.iter_mut().for_each(|v| *v = -20.0);
        spectrum.update(&settings.meters, false);
        assert!(spectrum.output_columns[0] < -20.0);
    }

    #[test]
    fn heights_span_floor_to_top() {
        assert_eq!(spectrum_height(FLOOR_DB), 0.0);
        assert_eq!(spectrum_height(SPECTRUM_TOP_DB), 1.0);
    }
}
