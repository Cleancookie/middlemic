//! The stage list, laid out like an FX chain rack: one framed row per stage with a fold
//! arrow, bypass switch, name and live readout, and the stage's controls when unfolded.

use crate::settings::{Param, Section, Settings, moved};
use egui::{Align, Color32, CursorIcon, Layout, Rect, Response, Sense, Stroke, Ui, vec2};
use std::sync::atomic::Ordering;

/// Gain-reduction bars span 0..this many dB.
const REDUCTION_RANGE_DB: f32 = 24.0;
/// Below this the header shows nothing, so it only lights up when the stage is working.
const REDUCTION_SHOWN_DB: f32 = 0.5;

/// Meter readings reset when taken, and egui may run the UI more than once per frame, so
/// take them once per frame and give every pass the same values.
fn reductions_this_frame(ui: &Ui, section: &Section) -> Vec<f32> {
    let frame = ui.ctx().cumulative_frame_nr();
    let key = ui.make_persistent_id((section.name, "reduction-readings"));
    ui.ctx().data_mut(|data| {
        let (taken_in, readings) = data.get_temp_mut_or_insert_with(key, || (u64::MAX, Vec::<f32>::new()));
        if *taken_in != frame {
            *taken_in = frame;
            *readings = (0..section.reduction_labels.len())
                .map(|slot| section.take_reduction(slot))
                .collect();
        }
        readings.clone()
    })
}

/// The parts of a row you can interact with, so callers and tests can find them.
#[cfg_attr(not(test), allow(dead_code))]
pub struct RackHeader {
    pub grip: Response,
    pub arrow: Response,
    pub power: Response,
    /// The whole row, header and body.
    pub rect: Rect,
}

/// Three bars to grab. Drawn, not a font glyph, so it never shows up as a missing-character box.
fn grip_handle(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::drag());
    let active = response.hovered() || response.dragged();
    let color = if active { Color32::WHITE } else { Color32::GRAY };
    for dy in [-4.0, 0.0, 4.0] {
        ui.painter().line_segment(
            [rect.center() + vec2(-5.0, dy), rect.center() + vec2(5.0, dy)],
            Stroke::new(1.5, color),
        );
    }
    response
        .on_hover_cursor(CursorIcon::Grab)
        .on_hover_text("Drag to reorder")
}

fn fold_arrow(ui: &mut Ui, open: bool) -> Response {
    let (_, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    egui::collapsing_header::paint_default_icon(ui, if open { 1.0 } else { 0.0 }, &response);
    response.on_hover_text("Fold or unfold")
}

/// Which gap between the rows (`0..=rows.len()`) the pointer is in: before the first row
/// whose middle is below it.
pub fn drop_gap(pointer_y: f32, rows: &[Rect]) -> usize {
    rows.iter().take_while(|r| r.center().y < pointer_y).count()
}

/// Where to draw the insertion line for `gap`. `rows` must not be empty.
fn gap_line_y(gap: usize, rows: &[Rect]) -> f32 {
    match gap {
        0 => rows[0].top(),
        g if g >= rows.len() => rows[rows.len() - 1].bottom(),
        g => (rows[g - 1].bottom() + rows[g].top()) / 2.0,
    }
}

/// One row of the rack. `body` runs only when unfolded and gets the gain reduction (dB) of
/// each of the stage's meters.
pub fn rack_item(ui: &mut Ui, section: &Section, body: impl FnOnce(&mut Ui, &[f32])) -> RackHeader {
    let reductions = reductions_this_frame(ui, section);
    let worst = reductions.iter().copied().fold(0.0, f32::max);
    let open = section.open.load(Ordering::Relaxed);

    let shown = egui::Frame::group(ui.style())
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = ui
                .horizontal(|ui| {
                    let grip = grip_handle(ui);
                    let arrow = fold_arrow(ui, open);

                    let mut on = section.is_enabled();
                    let power = ui.checkbox(&mut on, "").on_hover_text("On / bypass");
                    if power.changed() {
                        section.enabled.store(on, Ordering::Relaxed);
                    }

                    let mut name = egui::RichText::new(section.name).strong();
                    if !on {
                        name = name.color(Color32::GRAY);
                    }
                    let title = ui.add(egui::Label::new(name).sense(egui::Sense::click()));
                    if arrow.clicked() || title.clicked() {
                        section.open.store(!open, Ordering::Relaxed);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Reset").clicked() {
                            section.reset();
                        }
                        if !section.simple.is_empty() {
                            let mut advanced = section.advanced.load(Ordering::Relaxed);
                            if ui.checkbox(&mut advanced, "Advanced").changed() {
                                section.advanced.store(advanced, Ordering::Relaxed);
                            }
                        }
                        if on && worst >= REDUCTION_SHOWN_DB {
                            ui.colored_label(Color32::from_rgb(250, 170, 70), format!("-{worst:.1} dB"));
                        }
                    });
                    RackHeader { grip, arrow, power, rect: Rect::NOTHING }
                })
                .inner;
            if open {
                ui.separator();
                body(ui, &reductions);
            }
            header
        });
    RackHeader { rect: shown.response.rect, ..shown.inner }
}

/// The list of stages, in their running order, with drag-and-drop reordering.
#[derive(Default)]
pub struct Rack {
    /// Position of the row being dragged.
    dragging: Option<usize>,
    /// Where each row and its grip were drawn last frame, in running order.
    rows: Vec<Rect>,
    grips: Vec<Rect>,
}

impl Rack {
    /// Draws every stage as a row. `body` draws the controls of an unfolded row.
    pub fn show(&mut self, ui: &mut Ui, settings: &Settings, mut body: impl FnMut(&mut Ui, &Section, &[f32])) {
        self.rows.clear();
        self.grips.clear();
        for (position, &index) in settings.order.get().iter().enumerate() {
            let section = &settings.sections[index];
            let header = rack_item(ui, section, |ui, reductions| body(ui, section, reductions));
            self.rows.push(header.rect);
            self.grips.push(header.grip.rect);
            if header.grip.drag_started() {
                self.dragging = Some(position);
            }
        }
        self.finish_drag(ui, settings);
    }

    /// While a row is held, show where it would land; on release, move it there.
    fn finish_drag(&mut self, ui: &mut Ui, settings: &Settings) {
        let Some(from) = self.dragging else { return };
        let (pointer, released, cancelled) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.any_released(),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let (Some(pointer), false) = (pointer, self.rows.is_empty()) else {
            self.dragging = None;
            return;
        };
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let gap = drop_gap(pointer.y, &self.rows);
        let y = gap_line_y(gap, &self.rows);
        ui.painter().hline(
            self.rows[0].x_range(),
            y,
            Stroke::new(3.0, Color32::from_rgb(90, 160, 255)),
        );
        if released || cancelled {
            if released && !cancelled {
                settings.order.set(&moved(&settings.order.get(), from, gap));
            }
            self.dragging = None;
        }
    }
}

/// The plain controls: gain-reduction bars, quick picks and sliders.
pub fn section_body(ui: &mut Ui, section: &Section, reductions: &[f32]) {
    let status = section.status();
    if !status.is_empty() {
        ui.colored_label(Color32::LIGHT_RED, status);
    }
    if !reductions.is_empty() {
        egui::Grid::new((section.name, "reduction")).num_columns(2).show(ui, |ui| {
            for (label, db) in section.reduction_labels.iter().zip(reductions) {
                ui.label(format!("{label} turned down"));
                ui.add(
                    egui::ProgressBar::new(db / REDUCTION_RANGE_DB)
                        .desired_width(220.0)
                        .text(format!("{db:.1} dB")),
                );
                ui.end_row();
            }
        });
    }
    ui.add_enabled_ui(section.is_enabled(), |ui| {
        ui.spacing_mut().slider_width = 220.0;
        let shown: &[Param] = if section.is_advanced() {
            &section.params
        } else {
            &section.simple
        };
        if !section.is_advanced() && !section.quick_picks.is_empty() {
            ui.horizontal(|ui| {
                for (label, value) in &section.quick_picks {
                    if ui.button(*label).clicked() {
                        section.simple[0].set(*value);
                    }
                }
            });
        }
        for p in shown {
            let mut v = p.get();
            let slider = egui::Slider::new(&mut v, p.min..=p.max)
                .text(p.name)
                .suffix(format!(" {}", p.unit))
                .logarithmic(p.log);
            if ui.add(slider).changed() {
                p.set(v);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, Pos2, PointerButton, Rect, pos2, vec2};
    use std::cell::{Cell, RefCell};

    /// Runs frames of a one-row rack and remembers where the header controls were.
    struct Harness {
        ctx: egui::Context,
        arrow: Cell<Rect>,
        power: Cell<Rect>,
        body_drawn: Cell<bool>,
        shown_reductions: RefCell<Vec<f32>>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                arrow: Cell::new(Rect::NOTHING),
                power: Cell::new(Rect::NOTHING),
                body_drawn: Cell::new(false),
                shown_reductions: RefCell::new(Vec::new()),
            }
        }

        fn frame(&self, section: &Section, events: Vec<Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700.0, 500.0))),
                events,
                ..Default::default()
            };
            self.body_drawn.set(false);
            let _ = self.ctx.run_ui(input, |ui| {
                let header = rack_item(ui, section, |ui, reductions| {
                    self.body_drawn.set(true);
                    *self.shown_reductions.borrow_mut() = reductions.to_vec();
                    section_body(ui, section, reductions);
                });
                self.arrow.set(header.arrow.rect);
                self.power.set(header.power.rect);
            });
        }

        fn click(&self, section: &Section, at: Pos2) {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(section, vec![Event::PointerMoved(at)]);
            self.frame(section, vec![button(true)]);
            self.frame(section, vec![button(false)]);
            self.frame(section, vec![]);
        }
    }

    /// Three folded rows, A B C, and the mouse.
    struct Rows {
        ctx: egui::Context,
        rack: RefCell<Rack>,
        settings: std::sync::Arc<Settings>,
    }

    impl Rows {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                rack: RefCell::default(),
                settings: Settings::new(["A", "B", "C"].map(|n| Section::new(n, vec![])).into()),
            }
        }

        fn frame(&self, events: Vec<Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700.0, 500.0))),
                events,
                ..Default::default()
            };
            let _ = self.ctx.run_ui(input, |ui| {
                self.rack.borrow_mut().show(ui, &self.settings, |ui, section, r| section_body(ui, section, r));
            });
        }

        fn grip(&self, position: usize) -> Pos2 {
            self.rack.borrow().grips[position].center()
        }

        fn row(&self, position: usize) -> Rect {
            self.rack.borrow().rows[position]
        }

        /// Press on `from`, drag to `to`, and release (or press Escape instead).
        fn drag(&self, from: Pos2, to: Pos2, escape: bool) {
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(from)]);
            self.frame(vec![button(from, true)]);
            self.frame(vec![Event::PointerMoved(from + (to - from) * 0.5)]);
            self.frame(vec![Event::PointerMoved(to)]);
            if escape {
                self.frame(vec![Event::Key {
                    key: egui::Key::Escape,
                    physical_key: Some(egui::Key::Escape),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }]);
            }
            self.frame(vec![button(to, false)]);
            self.frame(vec![]);
        }
    }

    #[test]
    fn dragging_a_grip_to_the_bottom_moves_that_stage_last() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let to = pos2(rows.grip(0).x, rows.row(2).bottom() + 6.0);
        rows.drag(rows.grip(0), to, false);
        assert_eq!(rows.settings.order.get(), vec![1, 2, 0]);

        // The list is now drawn in the new order: the old first stage is last.
        rows.frame(vec![]);
        assert!(rows.row(0).top() < rows.row(2).top());
    }

    #[test]
    fn dragging_the_last_stage_to_the_top_moves_it_first() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let to = pos2(rows.grip(2).x, rows.row(0).top() - 3.0);
        rows.drag(rows.grip(2), to, false);
        assert_eq!(rows.settings.order.get(), vec![2, 0, 1]);
    }

    #[test]
    fn dropping_a_stage_back_where_it_was_changes_nothing() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let version = rows.settings.order.version();
        let to = pos2(rows.grip(1).x, rows.row(1).center().y + 4.0);
        rows.drag(rows.grip(1), to, false);
        assert_eq!(rows.settings.order.get(), vec![0, 1, 2]);
        assert_eq!(rows.settings.order.version(), version, "nothing for the audio thread to do");
    }

    #[test]
    fn escape_cancels_a_drag() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let to = pos2(rows.grip(0).x, rows.row(2).bottom() + 6.0);
        rows.drag(rows.grip(0), to, true);
        assert_eq!(rows.settings.order.get(), vec![0, 1, 2]);
    }

    #[test]
    fn pressing_on_the_name_does_not_start_a_drag() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let name = rows.row(0).center();
        let to = pos2(name.x, rows.row(2).bottom() + 6.0);
        rows.drag(name, to, false);
        assert_eq!(rows.settings.order.get(), vec![0, 1, 2], "only the grip drags");
    }

    #[test]
    fn the_insertion_gap_follows_the_pointer() {
        let rows: Vec<Rect> = (0..3)
            .map(|i| Rect::from_min_size(pos2(0.0, i as f32 * 40.0), vec2(100.0, 36.0)))
            .collect();
        assert_eq!(drop_gap(-5.0, &rows), 0);
        assert_eq!(drop_gap(30.0, &rows), 1, "past the middle of row 0");
        assert_eq!(drop_gap(50.0, &rows), 1, "still above the middle of row 1");
        assert_eq!(drop_gap(500.0, &rows), 3);
        assert_eq!(gap_line_y(0, &rows), 0.0);
        assert_eq!(gap_line_y(1, &rows), 38.0, "halfway between row 0's bottom and row 1's top");
        assert_eq!(gap_line_y(3, &rows), 116.0);
    }

    fn compressor_like() -> Section {
        Section::new("Compressor", vec![Param::new("Ratio", ":1", 1.0, 10.0, 2.0)])
            .with_reduction_meters(&["Mid"])
            .unfolded()
    }

    #[test]
    fn unfolded_row_shows_its_controls_and_folded_row_does_not() {
        let section = compressor_like();
        let h = Harness::new();
        h.frame(&section, vec![]);
        assert!(h.body_drawn.get());

        section.open.store(false, Ordering::Relaxed);
        h.frame(&section, vec![]);
        assert!(!h.body_drawn.get(), "folded rows draw no body");
    }

    #[test]
    fn clicking_the_arrow_folds_and_unfolds() {
        let section = compressor_like();
        let h = Harness::new();
        h.frame(&section, vec![]);
        h.click(&section, h.arrow.get().center());
        assert!(!section.open.load(Ordering::Relaxed), "first click folds");
        assert!(!h.body_drawn.get());

        h.click(&section, h.arrow.get().center());
        assert!(section.open.load(Ordering::Relaxed), "second click unfolds");
        assert!(h.body_drawn.get());
    }

    #[test]
    fn clicking_the_power_box_bypasses_without_folding() {
        let section = compressor_like();
        let h = Harness::new();
        h.frame(&section, vec![]);
        h.click(&section, h.power.get().center());
        assert!(!section.is_enabled(), "bypassed");
        assert!(section.open.load(Ordering::Relaxed), "still unfolded");

        h.click(&section, h.power.get().center());
        assert!(section.is_enabled());
    }

    #[test]
    fn a_folded_row_still_reports_and_resets_meters_each_frame() {
        let section = compressor_like();
        section.open.store(false, Ordering::Relaxed);
        section.record_reduction(0, 7.0);
        let h = Harness::new();
        h.frame(&section, vec![]);
        assert_eq!(section.take_reduction(0), 0.0, "the header consumed the reading");

        section.open.store(true, Ordering::Relaxed);
        section.record_reduction(0, 4.5);
        h.frame(&section, vec![]);
        assert_eq!(*h.shown_reductions.borrow(), vec![4.5], "the body receives it");
    }

    #[test]
    fn the_stage_list_has_a_row_for_every_stage_in_signal_order() {
        let settings = crate::settings::Settings::new(crate::dsp::default_sections());
        let names: Vec<_> = settings.sections.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "Echo cancellation",
                "Noise removal",
                "Noise removal (DPDFNet)",
                "Noise removal (RNNoise)",
                "Mic boost",
                "EQ",
                "De-esser",
                "Compressor",
                "Limiter"
            ]
        );
        let ctx = egui::Context::default();
        let drawn = std::cell::Cell::new(0);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            for s in &settings.sections {
                rack_item(ui, s, |ui, r| {
                    drawn.set(drawn.get() + 1);
                    section_body(ui, s, r);
                });
            }
        });
        assert_eq!(drawn.get(), 3, "only Noise removal, Mic boost and EQ start unfolded");
    }
}
