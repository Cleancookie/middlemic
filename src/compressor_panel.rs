//! The Compressor's Advanced view, after FL Studio's Maximus: a band graph with draggable
//! crossovers over the output spectrum, MASTER / LOW / MID / HIGH tabs, and for the open tab
//! its level meters, knobs, switches and transfer curve.

use crate::dsp::multiband::{BAND_HELP, BAND_TITLES, HIGH_XOVER, LOOKAHEAD, LOW_XOVER, MASTER, curve, field, param};
use crate::knob::knob;
use crate::rack::{once_per_frame, section_body};
use crate::settings::{Param, Section};
use crate::spectrum::{freq_to_x, x_to_freq};
use crate::theme;
use egui::{Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};

/// Strips in tab order.
const TABS: [usize; 4] = [MASTER, 0, 1, 2];
const TAB_LABELS: [&str; 4] = ["MASTER", "LOW", "MID", "HIGH"];
const DETECT_TIPS: [(&str, &str); 2] = [
    ("Peak detection", "Reacts to the loudest instant of each sound, so quick spikes such as a shout or a click are caught. Can sound grabby."),
    ("RMS detection", "Follows the average loudness over about 10 ms, closer to how you hear it. Smoother on voice; a good place to start."),
];
/// Graph height as a share of the width, clamped (points).
const GRAPH_HEIGHT: (f32, f32, f32) = (0.2, 90.0, 150.0);
const MIN_WIDTH: f32 = 200.0;
const METER_HEIGHT: f32 = 118.0;
const METER_WIDTH: f32 = 10.0;
const CURVE_SIZE: f32 = 132.0;
/// Meters and the transfer curve span this many dB below full scale.
const RANGE_DB: f32 = 60.0;
/// Gain-reduction bars span 0..this many dB.
const REDUCTION_RANGE_DB: f32 = 24.0;
const GRID_FREQS: [(f32, &str); 8] = [
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1_000.0, "1k"),
    (2_000.0, "2k"),
    (5_000.0, "5k"),
    (10_000.0, "10k"),
];

/// Low, mid and high take three accents from the theme ramp; the master is plain.
fn strip_color(strip: usize) -> Color32 {
    if strip == MASTER { theme::TEXT } else { theme::accent(strip, 3) }
}

fn switch_on(p: &Param) -> bool {
    p.get() >= 0.5
}

fn flip(p: &Param) {
    p.set(if switch_on(p) { 0.0 } else { 1.0 });
}

fn level_db(peak: f32) -> f32 {
    20.0 * peak.max(1e-6).log10()
}

/// 0 at `-RANGE_DB`, 1 at full scale.
fn level_norm(db: f32) -> f32 {
    (db / RANGE_DB + 1.0).clamp(0.0, 1.0)
}

/// Where things were drawn, so tests can aim the mouse.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Drawn {
    pub graph: Rect,
    pub tab: usize,
}

/// `spectrum` is the output spectrum as heights 0..1 along `freq_to_x`, or empty when stopped.
pub fn ui(ui: &mut Ui, section: &Section, reductions: &[f32], spectrum: &[f32]) -> Drawn {
    if !section.is_advanced() {
        section_body(ui, section, &reductions[..MASTER.min(reductions.len())]);
        return Drawn { graph: Rect::NOTHING, tab: MASTER };
    }
    let tab_id = ui.make_persistent_id("compressor-tab");
    let mut tab = ui.data_mut(|d| *d.get_persisted_mut_or(tab_id, MASTER));
    let levels = once_per_frame(ui, "compressor-levels", || {
        (0..section.level_meters()).map(|slot| section.take_level(slot)).collect()
    });
    let reductions: Vec<f32> = reductions
        .iter()
        .enumerate()
        .map(|(i, &db)| crate::motion::glide_reduction(ui.ctx(), tab_id.with(("reduction", i)), db))
        .collect();
    let reduction = |strip: usize| reductions.get(strip).copied().unwrap_or(0.0);
    let level = |slot: usize| levels.get(slot).copied().unwrap_or(0.0);
    let mut graph = Rect::NOTHING;
    ui.add_enabled_ui(section.is_enabled(), |ui| {
        graph = band_graph(ui, section, &reduction, spectrum, &mut tab);
        let colors = TABS.map(strip_color);
        let open = TABS.iter().position(|&s| s == tab).unwrap_or(0);
        let tips = TABS.map(|strip| (BAND_TITLES[strip], BAND_HELP[strip]));
        if let Some(i) = segmented(ui, ui.make_persistent_id("tabs"), &TAB_LABELS, &colors, open, &tips) {
            tab = TABS[i];
        }
        ui.add_space(4.0);
        strip_body(ui, section, tab, reduction(tab), [level(2 * tab), level(2 * tab + 1)]);
    });
    ui.data_mut(|d| d.insert_persisted(tab_id, tab));
    Drawn { graph, tab }
}

fn band_graph(ui: &mut Ui, section: &Section, reduction: &impl Fn(usize) -> f32, spectrum: &[f32], tab: &mut usize) -> Rect {
    let (share, min, max) = GRAPH_HEIGHT;
    let width = ui.available_width().max(MIN_WIDTH);
    let (rect, response) = ui.allocate_exact_size(vec2(width, (width * share).clamp(min, max)), Sense::click());
    let painter = ui.painter_at(rect);
    let on = ui.is_enabled();
    let x_of = |freq: f32| rect.left() + freq_to_x(freq) * rect.width();
    painter.rect_filled(rect, 6.0, theme::PLOT_BG);

    for (freq, label) in GRID_FREQS {
        let x = x_of(freq);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, theme::GRID));
        painter.text(pos2(x + 2.0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, label, FontId::proportional(10.0), theme::TEXT_DIM);
    }
    let column = rect.width() / spectrum.len().max(1) as f32;
    for (i, &h) in spectrum.iter().enumerate().filter(|(_, h)| **h > 0.0) {
        let x = rect.left() + i as f32 * column;
        let top = rect.bottom() - h * rect.height();
        painter.rect_filled(
            Rect::from_min_max(pos2(x, top), pos2(x + column + 0.5, rect.bottom())),
            0.0,
            theme::SPECTRUM_IN.gamma_multiply(0.18),
        );
    }

    let edges = [rect.left(), x_of(section.get(LOW_XOVER)), x_of(section.get(HIGH_XOVER)), rect.right()];
    for (band, label) in TAB_LABELS[1..].iter().enumerate() {
        let region = Rect::from_x_y_ranges(edges[band]..=edges[band + 1], rect.y_range()).shrink(1.0);
        let p = |f| &section.params[param(band, f)];
        let silent = switch_on(p(field::MUTE)) || switch_on(p(field::BYPASS));
        let selected = theme::animate(ui.ctx(), ui.make_persistent_id(("band-selected", band)), *tab == band);
        let color = if on && !silent { strip_color(band) } else { theme::OUTLINE_OFF };
        painter.rect_filled(region, 4.0, color.gamma_multiply(0.05 + 0.12 * selected));
        painter.rect_stroke(region, 4.0, Stroke::new(1.0, color.gamma_multiply(0.25 + 0.75 * selected)), StrokeKind::Inside);
        painter.text(region.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, *label, FontId::proportional(10.0), color);

        let db = reduction(band);
        let bar_width = (region.width() * 0.25).clamp(4.0, 16.0);
        let depth = (db / REDUCTION_RANGE_DB).clamp(0.0, 1.0) * (region.height() - 24.0);
        let top = region.top() + 20.0;
        let bar = Rect::from_min_size(pos2(region.center().x - bar_width / 2.0, top), vec2(bar_width, depth));
        painter.rect_filled(bar, 2.0, color.gamma_multiply(0.85));
        if db >= 0.1 {
            painter.text(bar.right_top() + vec2(4.0, 0.0), Align2::LEFT_TOP, format!("-{db:.1}"), FontId::proportional(10.0), color);
        }
    }
    let band_at = |x: f32| if x < edges[1] { 0 } else if x < edges[2] { 1 } else { 2 };
    if let Some(x) = response.interact_pointer_pos().filter(|_| response.clicked()).map(|p| p.x) {
        *tab = band_at(x);
    }
    if let Some(band) = response.hover_pos().map(|p| band_at(p.x)) {
        theme::help_tooltip_with_hint(response, BAND_TITLES[band], BAND_HELP[band], "Click to open its controls. The bar shows how much it is turned down.");
    }

    for (handle, index) in [LOW_XOVER, HIGH_XOVER].into_iter().enumerate() {
        let p = &section.params[index];
        let x = edges[handle + 1];
        let response = ui
            .interact(
                Rect::from_center_size(pos2(x, rect.center().y), vec2(14.0, rect.height())),
                ui.make_persistent_id(("crossover", handle)),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(CursorIcon::ResizeHorizontal);
        if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.dragged()) {
            p.set(x_to_freq((pointer.x - rect.left()) / rect.width()));
        }
        if response.double_clicked() {
            p.reset();
        }
        let hot = theme::animate(ui.ctx(), response.id.with("hot"), response.hovered() || response.dragged());
        let color = theme::GRID_STRONG.lerp_to_gamma(theme::TEXT, hot);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, color));
        let grip = Rect::from_center_size(pos2(x, rect.center().y), vec2(8.0, 24.0));
        painter.rect(grip, 3.0, theme::PANEL, Stroke::new(1.0 + hot, color), StrokeKind::Middle);
        theme::param_tooltip(response, p, &format!("Now {}. Drag to move, double-click resets", p.display()));
    }
    rect
}

/// A row of pill buttons with one highlight that slides and changes colour between them.
/// Returns the index clicked, if any.
fn segmented(ui: &mut Ui, id: Id, labels: &[&str], colors: &[Color32], selected: usize, tips: &[(&str, &str)]) -> Option<usize> {
    let font = FontId::proportional(11.0);
    let mut clicked = None;
    let cells: Vec<(Rect, f32)> = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            labels
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    let text = ui.painter().layout_no_wrap(label.to_string(), font.clone(), theme::TEXT);
                    let size = vec2(text.size().x + 22.0, 22.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    if response.clicked() {
                        clicked = Some(i);
                    }
                    if let Some((title, help)) = tips.get(i) {
                        theme::help_tooltip(response, title, help);
                    }
                    (rect,theme::animate(ui.ctx(), id.with(i), i == selected))
                })
                .collect()
        })
        .inner;
    let total: f32 = cells.iter().map(|(_, t)| t).sum::<f32>().max(1e-3);
    let blend = |f: &dyn Fn(&Rect) -> Pos2| {
        let sum = cells.iter().fold(vec2(0.0, 0.0), |acc, (r, t)| acc + f(r).to_vec2() * *t);
        (sum / total).to_pos2()
    };
    let highlight = Rect::from_min_max(blend(&|r| r.min), blend(&|r| r.max));
    let (mut color, mut weight) = (Color32::TRANSPARENT, 0.0);
    for ((_, t), c) in cells.iter().zip(colors).filter(|((_, t), _)| *t > 0.0) {
        weight += t;
        color = color.lerp_to_gamma(*c, t / weight);
    }
    let on = ui.is_enabled();
    let accent = if on { color } else { theme::OUTLINE_OFF };
    let painter = ui.painter();
    let all = cells.iter().fold(Rect::NOTHING, |acc, (r, _)| acc.union(*r));
    painter.rect_stroke(all, 11.0, Stroke::new(1.0, theme::GRID), StrokeKind::Inside);
    painter.rect(highlight, 11.0, accent.gamma_multiply(0.22), Stroke::new(1.0, accent), StrokeKind::Inside);
    for ((rect, t), (label, c)) in cells.iter().zip(labels.iter().zip(colors)) {
        let text = theme::TEXT_DIM.lerp_to_gamma(if on { *c } else { theme::TEXT_DIM }, *t);
        painter.text(rect.center(), Align2::CENTER_CENTER, *label, font.clone(), text);
    }
    clicked
}

/// A small square switch, filled in `color` when on.
fn switch(ui: &mut Ui, p: &Param, label: &str, color: Color32) {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    if response.clicked() {
        flip(p);
    }
    let t = theme::animate(ui.ctx(), response.id, switch_on(p));
    let color = if ui.is_enabled() { color } else { theme::OUTLINE_OFF };
    let outline = theme::GRID_STRONG.lerp_to_gamma(color, t.max(if response.hovered() { 0.5 } else { 0.0 }));
    let painter = ui.painter();
    painter.rect(rect, 5.0, color.gamma_multiply(0.9 * t), Stroke::new(1.0, outline), StrokeKind::Inside);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), theme::TEXT_DIM.lerp_to_gamma(theme::BG, t));
    theme::param_tooltip(response, p, "");
}

fn strip_body(ui: &mut Ui, section: &Section, strip: usize, reduction: f32, [input, output]: [f32; 2]) {
    let p = |f| &section.params[param(strip, f)];
    // Per strip, so switching tabs glides from that strip's own last reading.
    let norm = |slot: &str, peak: f32| {
        crate::motion::glide_level(ui.ctx(), ui.make_persistent_id(("strip-level", strip, slot)), level_norm(level_db(peak)))
    };
    let (input, output) = (norm("in", input), norm("out", output));
    let color = strip_color(strip);
    ui.horizontal_wrapped(|ui| {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                meter_column(ui, p(field::INPUT), "In", input, Some(p(field::THRESHOLD).get()), color);
                reduction_meter(ui, reduction);
                meter_column(ui, p(field::OUTPUT), "Out", output, None, color);
            });
        });
        ui.group(|ui| {
            ui.vertical(|ui| {
                let row = |ui: &mut Ui, knobs: &[(usize, &str)]| {
                    ui.horizontal(|ui| {
                        for &(f, label) in knobs {
                            knob(ui, p(f), label, color);
                        }
                    });
                };
                row(ui, &[(field::THRESHOLD, "Threshold"), (field::KNEE, "Knee"), (field::RATIO, "Ratio")]);
                row(ui, &[(field::ATTACK, "Attack"), (field::HOLD, "Hold"), (field::RELEASE, "Release")]);
                ui.horizontal(|ui| {
                    // Short labels: longer ones overlap at the 58 pt knob dial width; tooltips name them in full.
                    knob(ui, p(field::FLOOR), "Floor", color);
                    knob(ui, p(field::FLOOR_RATIO), "Fl ratio", color);
                    knob(ui, &section.params[LOOKAHEAD], "Ahead", theme::TEXT_DIM);
                });
            });
        });
        ui.group(|ui| {
            ui.vertical(|ui| {
                transfer_curve(ui, section, strip, input, reduction, color);
                ui.horizontal(|ui| {
                    let rms = p(field::RMS);
                    let picked = segmented(ui, ui.make_persistent_id("detect"), &["PEAK", "RMS"], &[color, color], switch_on(rms) as usize, &DETECT_TIPS);
                    if let Some(i) = picked {
                        rms.set(i as f32);
                    }
                    if strip != MASTER {
                        switch(ui, p(field::SOLO), "S", theme::WARN);
                        switch(ui, p(field::MUTE), "M", theme::ERROR);
                        switch(ui, p(field::BYPASS), "B", theme::TEXT_DIM);
                    }
                });
            });
        });
    });
}

/// A vertical level meter over its gain knob. `mark_db` draws a tick, such as the threshold.
fn meter_column(ui: &mut Ui, gain: &Param, label: &str, norm: f32, mark_db: Option<f32>, color: Color32) {
    ui.vertical(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(crate::knob::WIDTH, METER_HEIGHT), Sense::hover());
        let bar = Rect::from_center_size(rect.center(), vec2(METER_WIDTH, rect.height()));
        let painter = ui.painter();
        painter.rect(bar, 3.0, theme::PLOT_BG, Stroke::new(1.0, theme::GRID), StrokeKind::Outside);
        let fill = norm * bar.height();
        if fill > 0.5 {
            let lit = Rect::from_min_max(pos2(bar.left(), bar.bottom() - fill), bar.max);
            painter.rect_filled(lit, 3.0, if ui.is_enabled() { color } else { theme::OUTLINE_OFF });
        }
        if let Some(db) = mark_db {
            let y = bar.bottom() - level_norm(db) * bar.height();
            painter.line_segment([pos2(bar.left() - 4.0, y), pos2(bar.right() + 4.0, y)], Stroke::new(1.5, theme::TEXT));
        }
        knob(ui, gain, label, color);
    });
}

/// Gain reduction hanging from the top.
fn reduction_meter(ui: &mut Ui, db: f32) {
    ui.vertical(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(26.0, METER_HEIGHT), Sense::hover());
        let bar = Rect::from_center_size(rect.center(), vec2(METER_WIDTH, rect.height()));
        let painter = ui.painter();
        painter.rect(bar, 3.0, theme::PLOT_BG, Stroke::new(1.0, theme::GRID), StrokeKind::Outside);
        let depth = (db / REDUCTION_RANGE_DB).clamp(0.0, 1.0) * bar.height();
        if depth > 0.5 {
            painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width(), depth)), 3.0, theme::WARN);
        }
        // Fixed width: vertical_centered would claim the whole row and push Out off the edge.
        ui.allocate_ui_with_layout(vec2(rect.width(), 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("GR").size(9.5).color(theme::TEXT_DIM));
            ui.label(egui::RichText::new(format!("{db:.1}")).size(11.0).color(theme::TEXT_DIM));
        });
    });
}

/// Input dB across, output dB up, both over `RANGE_DB`, with the live level as a dot.
fn transfer_curve(ui: &mut Ui, section: &Section, strip: usize, norm: f32, reduction: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(CURVE_SIZE, CURVE_SIZE), Sense::hover());
    let painter = ui.painter_at(rect);
    let at = |input_db: f32, output_db: f32| {
        pos2(
            rect.left() + level_norm(input_db) * rect.width(),
            rect.bottom() - (output_db / RANGE_DB + 1.0) * rect.height(),
        )
    };
    painter.rect_filled(rect, 6.0, theme::PLOT_BG);
    for db in [-48.0, -36.0, -24.0, -12.0] {
        painter.line_segment([at(db, -RANGE_DB), at(db, 0.0)], Stroke::new(1.0, theme::GRID));
        painter.line_segment([at(-RANGE_DB, db), at(0.0, db)], Stroke::new(1.0, theme::GRID));
    }
    painter.line_segment([at(-RANGE_DB, -RANGE_DB), at(0.0, 0.0)], Stroke::new(1.0, theme::GRID_STRONG));

    let c = curve(section, strip);
    let color = if ui.is_enabled() { color } else { theme::OUTLINE_OFF };
    let dashed = |db: f32, stroke: Color32| {
        painter.extend(Shape::dashed_line(&[at(db, -RANGE_DB), at(db, 0.0)], Stroke::new(1.0, stroke), 3.0, 3.0));
    };
    dashed(c.threshold, color.gamma_multiply(0.6));
    if c.floor_ratio > 1.0 && c.floor > -RANGE_DB {
        dashed(c.floor, theme::TEXT_DIM.gamma_multiply(0.6));
    }
    let points: Vec<Pos2> = (0..=60)
        .map(|i| {
            let input = -RANGE_DB + i as f32;
            at(input, c.output_db(input))
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(2.0, color)));
    if norm > 0.0 {
        let input = (norm - 1.0) * RANGE_DB;
        let fade = (norm * 10.0).min(1.0);
        painter.circle_filled(at(input, input - reduction), 4.0, theme::TEXT.gamma_multiply(fade));
    }
    let small = FontId::proportional(9.5);
    painter.text(rect.right_bottom() - vec2(4.0, 3.0), Align2::RIGHT_BOTTOM, "IN", small.clone(), theme::TEXT_DIM);
    painter.text(rect.left_top() + vec2(4.0, 3.0), Align2::LEFT_TOP, "OUT", small, theme::TEXT_DIM);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::multiband;
    use egui::{Event, Modifiers, PointerButton};
    use std::sync::atomic::Ordering::Relaxed;

    fn advanced() -> Section {
        let s = multiband::section();
        s.advanced.store(true, Relaxed);
        s
    }

    fn frame(ctx: &egui::Context, section: &Section, events: Vec<Event>) -> (Drawn, usize) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 900.0))),
            events,
            ..Default::default()
        };
        let mut drawn = None;
        let spectrum = vec![0.5; 240];
        let out = ctx.run_ui(input, |ui| drawn = Some(super::ui(ui, section, &[3.0, 6.0, 0.0, 1.0], &spectrum)));
        (drawn.unwrap(), out.shapes.len())
    }

    fn press(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn click(ctx: &egui::Context, section: &Section, at: Pos2) -> Drawn {
        frame(ctx, section, vec![Event::PointerMoved(at)]);
        frame(ctx, section, vec![press(at, true)]);
        frame(ctx, section, vec![press(at, false)]);
        frame(ctx, section, vec![]).0
    }

    #[test]
    fn draws_every_tab_headless_without_changing_the_settings() {
        let section = advanced();
        section.record_level(0, 0.3);
        let before: Vec<f32> = section.params.iter().map(|p| p.get()).collect();
        let ctx = egui::Context::default();
        let (drawn, shapes) = frame(&ctx, &section, vec![]);
        assert_eq!(drawn.tab, MASTER);
        assert!(shapes > 60, "graph, tabs, meters, knobs and curve: {shapes}");
        assert!(drawn.graph.width() >= MIN_WIDTH);
        let after: Vec<f32> = section.params.iter().map(|p| p.get()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn simple_mode_shows_the_plain_controls() {
        let ctx = egui::Context::default();
        let (drawn, shapes) = frame(&ctx, &multiband::section(), vec![]);
        assert_eq!(drawn.graph, Rect::NOTHING);
        assert!(shapes > 0);
    }

    #[test]
    fn clicking_a_band_region_opens_its_tab() {
        let section = advanced();
        let ctx = egui::Context::default();
        let graph = frame(&ctx, &section, vec![]).0.graph;
        let x = |f: f32| graph.left() + freq_to_x(f) * graph.width();
        assert_eq!(click(&ctx, &section, pos2(x(1_000.0), graph.bottom() - 10.0)).tab, 1);
        assert_eq!(click(&ctx, &section, pos2(x(8_000.0), graph.bottom() - 10.0)).tab, 2);
        assert_eq!(click(&ctx, &section, pos2(x(60.0), graph.bottom() - 10.0)).tab, 0);
    }

    #[test]
    fn dragging_a_crossover_handle_moves_it() {
        let section = advanced();
        let ctx = egui::Context::default();
        let graph = frame(&ctx, &section, vec![]).0.graph;
        let low = &section.params[LOW_XOVER];
        let start = pos2(graph.left() + freq_to_x(low.get()) * graph.width(), graph.center().y);
        let end = start + vec2(40.0, 0.0);
        frame(&ctx, &section, vec![Event::PointerMoved(start)]);
        frame(&ctx, &section, vec![press(start, true)]);
        frame(&ctx, &section, vec![Event::PointerMoved(start + vec2(20.0, 0.0))]);
        frame(&ctx, &section, vec![Event::PointerMoved(end)]);
        frame(&ctx, &section, vec![press(end, false)]);
        assert!(low.get() > 250.0, "dragged right: {}", low.get());
        assert_eq!(section.params[HIGH_XOVER].get(), section.params[HIGH_XOVER].default);
    }

    #[test]
    fn meters_map_full_scale_to_the_top() {
        assert_eq!(level_norm(0.0), 1.0);
        assert_eq!(level_norm(-RANGE_DB), 0.0);
        assert_eq!(level_norm(level_db(0.0)), 0.0, "silence");
    }
}
