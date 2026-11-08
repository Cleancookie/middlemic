//! The stage list, laid out like an FX chain rack: one framed row per stage with a fold
//! arrow, bypass switch, name and live readout, and the stage's controls when unfolded.

use crate::icons::cross_button;
use crate::settings::{Item, Param, Removed, Section, Settings};
use crate::theme;
use egui::{Align, Align2, Color32, CursorIcon, FontId, Id, Layout, Rect, Response, Sense, Shape, Stroke, Ui, UiBuilder, pos2, vec2};
use std::f32::consts::FRAC_PI_2;
use std::sync::atomic::Ordering;

/// Gain-reduction bars span 0..this many dB.
const REDUCTION_RANGE_DB: f32 = 24.0;
/// Below this the header shows nothing, so it only lights up when the stage is working.
const REDUCTION_SHOWN_DB: f32 = 0.5;
/// How long "Undo" stays offered after a stage is removed.
const UNDO_SECONDS: f64 = 5.0;

/// Meter readings reset when taken, and egui may run the UI more than once per frame, so
/// `take` them once per frame and give every pass the same values.
pub fn once_per_frame(ui: &Ui, key: &str, take: impl FnOnce() -> Vec<f32>) -> Vec<f32> {
    let frame = ui.ctx().cumulative_frame_nr();
    let key = ui.make_persistent_id(key);
    ui.ctx().data_mut(|data| {
        let (taken_in, readings) = data.get_temp_mut_or_insert_with(key, || (u64::MAX, Vec::<f32>::new()));
        if *taken_in != frame {
            *taken_in = frame;
            *readings = take();
        }
        readings.clone()
    })
}

fn reductions_this_frame(ui: &Ui, section: &Section) -> Vec<f32> {
    once_per_frame(ui, "reduction-readings", || {
        (0..section.reduction_labels.len()).map(|slot| section.take_reduction(slot)).collect()
    })
}

/// The parts of a row you can interact with, so callers and tests can find them.
#[cfg_attr(not(test), allow(dead_code))]
pub struct RackHeader {
    pub grip: Response,
    pub arrow: Response,
    pub power: Response,
    pub remove: Response,
    /// The whole row, header and body.
    pub rect: Rect,
}

/// Three bars to grab. Drawn, not a font glyph, so it never shows up as a missing-character box.
fn grip_handle(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::drag());
    let active = response.hovered() || response.dragged();
    let color = if active { theme::TEXT } else { theme::TEXT_DIM };
    for dy in [-4.0, 0.0, 4.0] {
        ui.painter().line_segment(
            [rect.center() + vec2(-5.0, dy), rect.center() + vec2(5.0, dy)],
            Stroke::new(1.5, color),
        );
    }
    theme::help_tooltip(
        response.on_hover_cursor(CursorIcon::Grab),
        "Reorder",
        "Drag this handle up or down to change the order the stages run in. Each stage hears the output of the one above it.",
    )
}

/// A chevron that turns from pointing right to pointing down as `openness` goes 0 to 1.
fn fold_arrow(ui: &mut Ui, openness: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let color = if response.hovered() { theme::TEXT } else { theme::TEXT_DIM };
    let turn = egui::emath::Rot2::from_angle(openness * FRAC_PI_2);
    let points = [vec2(-2.5, -4.0), vec2(2.0, 0.0), vec2(-2.5, 4.0)].map(|p| rect.center() + turn * p);
    ui.painter().add(Shape::line(points.to_vec(), Stroke::new(1.8, color)));
    theme::help_tooltip(response, "Fold or unfold", "Shows or hides this stage's controls. Folding does not turn the stage off.")
}

/// The bypass switch: a ring that fills with a dot of the accent colour when the stage is on.
fn power_dot(ui: &mut Ui, on_t: f32, accent: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let ring = theme::OUTLINE_OFF.lerp_to_gamma(accent, on_t);
    ui.painter().circle_stroke(rect.center(), 6.0, Stroke::new(1.5, ring));
    ui.painter().circle_filled(rect.center(), 3.5 * on_t, accent);
    theme::help_tooltip(
        response,
        "Power",
        "Lit means the stage is on. Click to bypass it: the sound passes through untouched and it adds no delay. Handy for comparing with and without.",
    )
}

/// Gap between a row's header and its controls; it folds away with them.
const BODY_GAP: f32 = 8.0;

/// Draws `body` from `top` (the header's bottom edge), clipped to a height that follows
/// `openness` and fading in as it opens. The gap above the body is inside the animated
/// height and nothing else is allocated, so the row ends exactly at the header when shut.
fn fold_body(ui: &mut Ui, id: Id, top: f32, openness: f32, body: impl FnOnce(&mut Ui)) {
    let left = ui.max_rect().left();
    let width = ui.available_width();
    // Last frame's height only sets the clip; the first frame of a never-opened row is hidden.
    let known: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    let start = pos2(left, top);
    let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(start, vec2(width, f32::INFINITY))));
    if openness < 1.0 {
        let visible = Rect::from_min_size(start, vec2(width, known * openness));
        child.set_clip_rect(visible.intersect(ui.clip_rect()));
        child.set_opacity(openness);
    }
    child.add_space(BODY_GAP);
    body(&mut child);
    let full = child.min_rect().bottom() - top;
    ui.data_mut(|d| d.insert_temp(id, full));
    ui.allocate_rect(Rect::from_min_size(start, vec2(width, full * openness)), Sense::hover());
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

const ROW_RADIUS: u8 = 8;

/// One row of the rack, outlined in `accent` (grey when bypassed). `body` runs only when
/// unfolded and gets the gain reduction (dB) of each of the stage's meters. Widget ids inside
/// are scoped to the item, so two stages of one kind never share state.
pub fn rack_item(ui: &mut Ui, item: &Item, accent: Color32, body: impl FnOnce(&mut Ui, &[f32])) -> RackHeader {
    ui.push_id(("rack-item", item.id), |ui| item_row(ui, item, accent, body)).inner
}

fn item_row(ui: &mut Ui, item: &Item, accent: Color32, body: impl FnOnce(&mut Ui, &[f32])) -> RackHeader {
    let section = &item.section;
    let reductions = reductions_this_frame(ui, section);
    let worst = reductions.iter().copied().fold(0.0, f32::max);
    let open = section.open.load(Ordering::Relaxed);
    let on = section.is_enabled();
    let id = ui.make_persistent_id("row");
    let ctx = ui.ctx().clone();
    let open_t = theme::animate(&ctx, id.with("open"), open);
    let on_t = theme::animate(&ctx, id.with("on"), on);

    let shown = egui::Frame::new()
        .fill(theme::PANEL)
        .corner_radius(ROW_RADIUS)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = ui.horizontal(|ui| {
                    let grip = grip_handle(ui);
                    let arrow = fold_arrow(ui, open_t);
                    let power = power_dot(ui, on_t, accent);
                    if power.clicked() {
                        section.enabled.store(!on, Ordering::Relaxed);
                    }

                    let color = theme::TEXT_DIM.lerp_to_gamma(accent, on_t);
                    let name = egui::RichText::new(item.name.to_uppercase())
                        .strong()
                        .size(13.0)
                        .extra_letter_spacing(1.0)
                        .color(color);
                    let title = ui.add(egui::Label::new(name).sense(Sense::click()));
                    theme::help_tooltip_with_hint(title.clone(), &item.name, section.help, "Click to fold or unfold");
                    if arrow.clicked() || title.clicked() {
                        section.open.store(!open, Ordering::Relaxed);
                    }

                    let remove = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let remove = theme::help_tooltip(
                            cross_button(ui),
                            "Remove",
                            "Takes this stage out of the rack. An Undo link appears for a few seconds.",
                        );
                        let reset = ui.small_button("Reset");
                        if reset.clicked() {
                            section.reset();
                        }
                        theme::help_tooltip(
                            reset,
                            "Reset stage",
                            "Puts every control of this stage back to its starting value, including its on/off switch and Simple mode.",
                        );
                        if !section.simple.is_empty() {
                            let mut advanced = section.advanced.load(Ordering::Relaxed);
                            let toggle = ui.checkbox(&mut advanced, "Advanced");
                            if toggle.changed() {
                                section.advanced.store(advanced, Ordering::Relaxed);
                            }
                            theme::help_tooltip(
                                toggle,
                                "Advanced",
                                "Off: a single simple control does the job. On: every setting is yours to tune.",
                            );
                        }
                        if on && worst >= REDUCTION_SHOWN_DB {
                            ui.colored_label(theme::WARN, format!("-{worst:.1} dB"));
                        }
                        remove
                    });
                    RackHeader { grip, arrow, power, remove: remove.inner, rect: Rect::NOTHING }
                });
            if open || open_t > 0.0 {
                let top = header.response.rect.bottom();
                fold_body(ui, id.with("body"), top, open_t, |ui| body(ui, &reductions));
            }
            header.inner
        });

    let rect = shown.response.rect;
    let hover_t = theme::animate(&ctx, id.with("hover"), ui.rect_contains_pointer(rect));
    let outline = theme::OUTLINE_OFF.lerp_to_gamma(accent, on_t).lerp_to_gamma(theme::TEXT, 0.25 * hover_t);
    ui.painter().rect_stroke(rect, ROW_RADIUS, Stroke::new(1.5 + 0.5 * hover_t, outline), egui::StrokeKind::Inside);
    RackHeader { rect, ..shown.inner }
}

/// The `+` row under the stages: a dim outline that opens a menu of every kind of stage.
/// Returns where it was drawn and the kind picked.
fn add_row(ui: &mut Ui) -> (Rect, Option<&'static str>) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let hover_t = theme::animate(ui.ctx(), response.id.with("hover"), response.hovered());
    let color = theme::OUTLINE_OFF.lerp_to_gamma(theme::TEXT, 0.35 * hover_t);
    let mut outline = Vec::new();
    egui::epaint::tessellator::path::rounded_rectangle(
        &mut outline,
        egui::epaint::RoundedRect::new(rect.shrink(0.75), f32::from(ROW_RADIUS)),
    );
    outline.push(outline[0]);
    ui.painter().extend(Shape::dashed_line(&outline, Stroke::new(1.5, color), 6.0, 4.0));
    let text = theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, hover_t);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, "+", FontId::proportional(20.0), text);

    let mut picked = None;
    egui::Popup::menu(&response).show(|ui| {
        for kind in crate::dsp::kinds() {
            let entry = ui.button(kind);
            if entry.clicked() {
                picked = Some(kind);
            }
            if let Some(section) = crate::dsp::new_section(kind) {
                theme::help_tooltip(entry, kind, section.help);
            }
        }
    });
    theme::help_tooltip(
        response.on_hover_cursor(CursorIcon::PointingHand),
        "Add a stage",
        "Adds another effect to the bottom of the rack. You can have more than one of the same kind, and drag it to where you want it.",
    );
    (rect, picked)
}

/// The list of stages, in their running order, with drag-and-drop reordering, a `+` row to
/// add stages and a short-lived undo for removing one.
#[derive(Default)]
pub struct Rack {
    /// Position of the row being dragged.
    dragging: Option<usize>,
    /// Where each row and its grip were drawn last frame, in running order.
    rows: Vec<Rect>,
    grips: Vec<Rect>,
    removes: Vec<Rect>,
    plus: Option<Rect>,
    undo_button: Option<Rect>,
    /// The last stage removed, and until when (egui time) it can be put back.
    undo: Option<(Removed, f64)>,
}

impl Rack {
    /// Draws every stage as a row. `body` draws the controls of an unfolded row.
    pub fn show(&mut self, ui: &mut Ui, settings: &Settings, mut body: impl FnMut(&mut Ui, &Item, &[f32])) {
        self.rows.clear();
        self.grips.clear();
        self.removes.clear();
        let items = settings.items();
        let mut remove = None;
        for (position, item) in items.iter().enumerate() {
            let accent = theme::accent(position, items.len());
            let header = rack_item(ui, item, accent, |ui, reductions| body(ui, item, reductions));
            self.rows.push(header.rect);
            self.grips.push(header.grip.rect);
            self.removes.push(header.remove.rect);
            if header.grip.drag_started() {
                self.dragging = Some(position);
            }
            if header.remove.clicked() {
                remove = Some(item.id);
            }
        }
        self.finish_drag(ui, settings);
        let (plus, picked) = add_row(ui);
        self.plus = Some(plus);
        if let Some(kind) = picked {
            settings.add(crate::dsp::new_section(kind).expect("listed kinds exist"));
        }
        let now = ui.input(|i| i.time);
        if let Some(removed) = remove.and_then(|id| settings.remove(id)) {
            self.undo = Some((removed, now + UNDO_SECONDS));
        }
        self.undo_link(ui, settings, now);
    }

    fn undo_link(&mut self, ui: &mut Ui, settings: &Settings, now: f64) {
        let Some((removed, until)) = &self.undo else { return };
        if now >= *until {
            self.undo = None;
            return;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(until - now));
        let link = ui
            .horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Removed {}.", removed.item.name)).color(theme::TEXT_DIM));
                ui.link("Undo")
            })
            .inner;
        self.undo_button = Some(link.rect);
        if link.clicked() {
            if let Some((removed, _)) = self.undo.take() {
                settings.restore(removed);
            }
        }
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
            Stroke::new(3.0, theme::DEESSER),
        );
        if released || cancelled {
            if released && !cancelled {
                settings.move_item(from, gap);
            }
            self.dragging = None;
        }
    }
}

/// The plain controls: gain-reduction bars, quick picks and sliders.
pub fn section_body(ui: &mut Ui, section: &Section, reductions: &[f32]) {
    let status = section.status();
    if !status.is_empty() {
        ui.colored_label(theme::ERROR, status);
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
                for (label, value, help) in &section.quick_picks {
                    let pick = ui.button(*label);
                    if pick.clicked() {
                        section.simple[0].set(*value);
                    }
                    theme::help_tooltip(pick, label, help);
                }
            });
        }
        for p in shown {
            let mut v = p.get();
            // The label is drawn here rather than by `Slider::text` so it can carry the tooltip too.
            ui.horizontal(|ui| {
                let slider = egui::Slider::new(&mut v, p.min..=p.max)
                    .suffix(format!(" {}", p.unit))
                    .logarithmic(p.log);
                let response = ui.add(slider);
                if response.changed() {
                    p.set(v);
                }
                theme::param_tooltip(response, p, "");
                let name = ui.add(egui::Label::new(p.name).wrap_mode(egui::TextWrapMode::Extend));
                theme::param_tooltip(name, p, "");
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, Pos2, PointerButton, Rect, pos2, vec2};
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;

    /// Runs frames of a one-row rack and remembers where the header controls were.
    struct Harness {
        ctx: egui::Context,
        arrow: Cell<Rect>,
        power: Cell<Rect>,
        row: Cell<Rect>,
        body_drawn: Cell<bool>,
        shown_reductions: RefCell<Vec<f32>>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                arrow: Cell::new(Rect::NOTHING),
                power: Cell::new(Rect::NOTHING),
                row: Cell::new(Rect::NOTHING),
                body_drawn: Cell::new(false),
                shown_reductions: RefCell::new(Vec::new()),
            }
        }

        fn frame(&self, section: &Arc<Section>, events: Vec<Event>) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700.0, 500.0))),
                events,
                ..Default::default()
            };
            self.body_drawn.set(false);
            let item = Item { id: 1, name: section.name.to_string(), section: section.clone() };
            let _ = self.ctx.run_ui(input, |ui| {
                let header = rack_item(ui, &item, theme::accent(0, 1), |ui, reductions| {
                    self.body_drawn.set(true);
                    *self.shown_reductions.borrow_mut() = reductions.to_vec();
                    section_body(ui, section, reductions);
                });
                self.arrow.set(header.arrow.rect);
                self.power.set(header.power.rect);
                self.row.set(header.rect);
            });
        }

        /// Row heights over the frames after toggling the fold, until it settles.
        fn fold_heights(&self, section: &Arc<Section>) -> Vec<f32> {
            section.open.store(!section.open.load(Ordering::Relaxed), Ordering::Relaxed);
            (0..30)
                .map(|_| {
                    self.frame(section, vec![]);
                    self.row.get().height()
                })
                .collect()
        }

        fn click(&self, section: &Arc<Section>, at: Pos2) {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(section, vec![Event::PointerMoved(at)]);
            self.frame(section, vec![button(true)]);
            self.frame(section, vec![button(false)]);
            self.settle(section);
        }

        /// Runs long enough for the fold and fade animations to finish.
        fn settle(&self, section: &Arc<Section>) {
            (0..30).for_each(|_| self.frame(section, vec![]));
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
                self.rack.borrow_mut().show(ui, &self.settings, |ui, item, r| section_body(ui, &item.section, r));
            });
        }

        fn click(&self, at: Pos2) {
            let button = |pressed| Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(at)]);
            self.frame(vec![button(true)]);
            self.frame(vec![button(false)]);
            self.frame(vec![]);
        }

        fn names(&self) -> Vec<String> {
            self.settings.items().iter().map(|i| i.name.clone()).collect()
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
        assert_eq!(rows.names(), ["B", "C", "A"]);

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
        assert_eq!(rows.names(), ["C", "A", "B"]);
    }

    #[test]
    fn dropping_a_stage_back_where_it_was_changes_nothing() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let version = rows.settings.generation();
        let to = pos2(rows.grip(1).x, rows.row(1).center().y + 4.0);
        rows.drag(rows.grip(1), to, false);
        assert_eq!(rows.names(), ["A", "B", "C"]);
        assert_eq!(rows.settings.generation(), version, "nothing for the audio thread to do");
    }

    #[test]
    fn escape_cancels_a_drag() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let to = pos2(rows.grip(0).x, rows.row(2).bottom() + 6.0);
        rows.drag(rows.grip(0), to, true);
        assert_eq!(rows.names(), ["A", "B", "C"]);
    }

    #[test]
    fn pressing_on_the_name_does_not_start_a_drag() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let name = rows.row(0).center();
        let to = pos2(name.x, rows.row(2).bottom() + 6.0);
        rows.drag(name, to, false);
        assert_eq!(rows.names(), ["A", "B", "C"], "only the grip drags");
    }

    #[test]
    fn the_cross_removes_a_stage_and_undo_puts_it_back() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let remove = rows.rack.borrow().removes[1];
        assert!(remove.right() > rows.row(1).right() - 40.0, "at the right end of the header");
        rows.click(remove.center());
        assert_eq!(rows.names(), ["A", "C"]);
        assert_eq!(rows.rack.borrow().rows.len(), 2);

        let undo = rows.rack.borrow().undo_button.unwrap();
        assert!(undo.top() > rows.rack.borrow().plus.unwrap().bottom(), "offered under the rack");
        rows.click(undo.center());
        assert_eq!(rows.names(), ["A", "B", "C"]);
        assert!(rows.rack.borrow().undo.is_none());
    }

    #[test]
    fn the_undo_offer_runs_out() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let remove = rows.rack.borrow().removes[0];
        rows.click(remove.center());
        assert!(rows.rack.borrow().undo.is_some());
        let later = egui::RawInput { time: Some(UNDO_SECONDS + 60.0), ..Default::default() };
        let _ = rows.ctx.run_ui(later, |ui| {
            rows.rack.borrow_mut().show(ui, &rows.settings, |_, _, _| {});
        });
        assert!(rows.rack.borrow().undo.is_none());
        assert_eq!(rows.names(), ["B", "C"]);
    }

    #[test]
    fn the_plus_row_spans_the_rack_and_opens_a_menu() {
        let rows = Rows::new();
        rows.frame(vec![]);
        let plus = rows.rack.borrow().plus.unwrap();
        assert!(plus.top() >= rows.row(2).bottom(), "below the last stage");
        assert_eq!(plus.x_range(), rows.row(0).x_range(), "as wide as a row");
        assert!(!egui::Popup::is_any_open(&rows.ctx));
        rows.click(plus.center());
        assert!(egui::Popup::is_any_open(&rows.ctx));
    }

    #[test]
    fn two_stages_of_one_kind_fold_separately() {
        let settings = Settings::new(vec![crate::dsp::new_section("EQ").unwrap(), crate::dsp::new_section("EQ").unwrap()]);
        let items = settings.items();
        items[1].section.open.store(false, Ordering::Relaxed);
        let ctx = egui::Context::default();
        let mut rack = Rack::default();
        for _ in 0..30 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                rack.show(ui, &settings, |ui, item, r| section_body(ui, &item.section, r));
            });
        }
        assert!(rack.rows[0].height() > rack.rows[1].height() + 20.0, "{:?}", rack.rows);
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

    fn compressor_like() -> Arc<Section> {
        Arc::new(
            Section::new("Compressor", vec![Param::new("Ratio", ":1", 1.0, 10.0, 2.0)])
                .with_reduction_meters(&["Mid"])
                .unfolded(),
        )
    }

    #[test]
    fn unfolded_row_shows_its_controls_and_folded_row_does_not() {
        let section = compressor_like();
        let h = Harness::new();
        h.frame(&section, vec![]);
        assert!(h.body_drawn.get());

        section.open.store(false, Ordering::Relaxed);
        h.settle(&section);
        assert!(!h.body_drawn.get(), "folded rows draw no body");
    }

    #[test]
    fn folding_eases_into_its_end_height_without_a_jump() {
        let h = Harness::new();
        let section = compressor_like();
        h.settle(&section);
        let open_height = h.row.get().height();

        let closing = h.fold_heights(&section);
        let shut = *closing.last().unwrap();
        assert!(shut < open_height - 40.0, "the row really folds");
        assert!(closing.windows(2).all(|w| w[1] <= w[0] + 0.01), "only ever shrinks: {closing:?}");
        let last_move = closing.iter().rev().find(|&&x| (x - shut).abs() > 0.01).unwrap();
        assert!(last_move - shut < 3.0, "last step is eased, not a jump: {closing:?}");

        let opening = h.fold_heights(&section);
        assert!((opening.last().unwrap() - open_height).abs() < 0.01, "unfolds back to full height");
        assert!(opening.windows(2).all(|w| w[1] >= w[0] - 0.01), "only ever grows: {opening:?}");
        assert!(opening[0] - shut < 3.0, "first step is eased, not a jump: {opening:?}");
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
        let names: Vec<_> = settings.items().iter().map(|i| i.name.clone()).collect();
        assert_eq!(
            names,
            [
                "Feedback suppressor",
                "Echo cancellation",
                "Noise removal (DPDFNet)",
                "Noise removal",
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
            for item in settings.items() {
                rack_item(ui, &item, theme::accent(0, 1), |ui, r| {
                    drawn.set(drawn.get() + 1);
                    section_body(ui, &item.section, r);
                });
            }
        });
        assert_eq!(drawn.get(), 3, "only Noise removal, Mic boost and EQ start unfolded");
    }
}
