//! The stage list, laid out like an FX chain rack: one framed row per stage with a fold
//! arrow, bypass switch, name and live readout, and the stage's controls when unfolded.

use crate::settings::{Param, Section};
use egui::{Align, Color32, Layout, Response, Ui};
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

/// The parts of a row you can click, so callers and tests can find them.
#[cfg_attr(not(test), allow(dead_code))]
pub struct RackHeader {
    pub arrow: Response,
    pub power: Response,
}

/// One row of the rack. `body` runs only when unfolded and gets the gain reduction (dB) of
/// each of the stage's meters.
pub fn rack_item(ui: &mut Ui, section: &Section, body: impl FnOnce(&mut Ui, &[f32])) -> RackHeader {
    let reductions = reductions_this_frame(ui, section);
    let worst = reductions.iter().copied().fold(0.0, f32::max);
    let open = section.open.load(Ordering::Relaxed);

    egui::Frame::group(ui.style())
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = ui
                .horizontal(|ui| {
                    let arrow = ui
                        .add(egui::Button::new(if open { "▼" } else { "▶" }).frame(false))
                        .on_hover_text("Fold or unfold");

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
                            ui.colored_label(Color32::from_rgb(250, 170, 70), format!("▼ {worst:.1} dB"));
                        }
                    });
                    RackHeader { arrow, power }
                })
                .inner;
            if open {
                ui.separator();
                body(ui, &reductions);
            }
            header
        })
        .inner
}

/// The plain controls: gain-reduction bars, quick picks and sliders.
pub fn section_body(ui: &mut Ui, section: &Section, reductions: &[f32]) {
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
    use egui::{Event, Modifiers, Pos2, PointerButton, Rect, vec2};
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
            ["Noise removal", "Mic boost", "EQ", "De-esser", "Compressor", "Limiter"]
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
