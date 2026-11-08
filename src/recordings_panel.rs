//! The Record & playback section at the bottom of the rack. It only draws and returns what the
//! user asked for; `ui.rs` does the recording, playing and rendering.

use crate::icons::{play_button, record_button, stop_button};
use crate::theme;
use egui::{CollapsingHeader, ComboBox, Rect, RichText, Sense, Ui, vec2};
use std::path::{Path, PathBuf};

pub struct Take {
    pub path: PathBuf,
    pub name: String,
    pub seconds: f32,
}

pub struct View<'a> {
    pub takes: &'a [Take],
    /// Seconds recorded so far, while recording.
    pub recording: Option<f32>,
    /// The take that is playing and how far through it is, 0..1.
    pub playing: Option<(&'a Path, f32)>,
    /// The take being rendered and a progress line.
    pub rendering: Option<(&'a Path, &'a str)>,
    pub outputs: &'a [String],
    /// Where playback is heard.
    pub monitor: &'a mut String,
    pub looping: &'a mut bool,
    /// What went wrong or what just finished.
    pub message: &'a str,
}

pub enum Action {
    Record,
    StopRecording,
    Play(PathBuf),
    StopPlayback,
    Seek(f32),
    Render(PathBuf),
    ShowInFolder(PathBuf),
    Delete(PathBuf),
}

pub fn clock(seconds: f32) -> String {
    let whole = seconds as u32;
    format!("{}:{:02}", whole / 60, whole % 60)
}

pub fn ui(ui: &mut Ui, view: View) -> Option<Action> {
    let mut action = None;
    CollapsingHeader::new(RichText::new("Record & playback").strong()).id_salt("recordings").default_open(true).show(ui, |ui| {
        ui.label(
            RichText::new(
                "Record your raw mic, then play it back through the rack above: every knob you move is heard on it. \
                 Right-click a take to render it to a file.",
            )
            .color(theme::TEXT_DIM),
        );
        ui.horizontal(|ui| {
            if let Some(seconds) = view.recording {
                let stop = stop_button(ui);
                if theme::help_tooltip(stop, "Stop recording", "Saves the take.").clicked() {
                    action = Some(Action::StopRecording);
                }
                ui.colored_label(theme::ERROR, format!("Recording {}", clock(seconds)));
            } else {
                let record = record_button(ui, theme::ERROR);
                let help = "Records your raw microphone, before any processing, to a new take. Nothing is sent to the output.";
                if theme::help_tooltip(record, "Record", help).clicked() {
                    action = Some(Action::Record);
                }
                ui.label("Record a take");
            }
            ui.separator();
            ui.label("Hear it on");
            let shown = if view.monitor.is_empty() { crate::wizard::WINDOWS_DEFAULT } else { view.monitor.as_str() }.to_string();
            let picker = ComboBox::from_id_salt("monitor").width(180.0).truncate().selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(view.monitor, crate::wizard::WINDOWS_DEFAULT.to_string(), crate::wizard::WINDOWS_DEFAULT);
                for name in view.outputs {
                    ui.selectable_value(view.monitor, name.clone(), name);
                }
            });
            theme::help_tooltip(
                picker.response,
                "Hear it on",
                "Where playback is heard. Choose your headphones or speakers. Do not choose CABLE Input or Discord will hear it.",
            );
            ui.checkbox(view.looping, "Loop");
        });
        if !view.message.is_empty() {
            ui.label(RichText::new(view.message).color(theme::WARN));
        }
        if view.takes.is_empty() {
            ui.label(RichText::new("No takes yet.").color(theme::TEXT_DIM));
        }
        for take in view.takes {
            if let Some(found) = take_row(ui, take, &view) {
                action = Some(found);
            }
        }
    });
    action
}

fn take_row(ui: &mut Ui, take: &Take, view: &View) -> Option<Action> {
    let mut action = None;
    let mine = |p: &Path| p == take.path;
    let playing = view.playing.filter(|(p, _)| mine(p));
    let row = ui.horizontal(|ui| {
        if playing.is_some() {
            if stop_button(ui).clicked() {
                action = Some(Action::StopPlayback);
            }
        } else if theme::help_tooltip(play_button(ui), "Play", "Plays this take through the rack. Move any control while it plays to hear the difference.").clicked() {
            action = Some(Action::Play(take.path.clone()));
        }
        ui.label(&take.name);
        ui.label(RichText::new(clock(take.seconds)).color(theme::TEXT_DIM));
        if let Some((_, line)) = view.rendering.filter(|(p, _)| mine(p)) {
            ui.label(RichText::new(line).color(theme::WARN));
        }
    });
    row.response.interact(Sense::click()).context_menu(|ui| {
        if ui.add_enabled(view.rendering.is_none(), egui::Button::new("Render through the rack")).clicked() {
            action = Some(Action::Render(take.path.clone()));
            ui.close();
        }
        if ui.button("Show in folder").clicked() {
            action = Some(Action::ShowInFolder(take.path.clone()));
            ui.close();
        }
        if ui.button("Delete").clicked() {
            action = Some(Action::Delete(take.path.clone()));
            ui.close();
        }
    });
    if let Some((_, fraction)) = playing {
        let (rect, bar) = ui.allocate_exact_size(vec2(ui.available_width().min(520.0), 8.0), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 3.0, theme::GRID);
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(rect.width() * fraction, rect.height())), 3.0, theme::accent(3, 4));
        if let (true, Some(pointer)) = (bar.clicked() || bar.dragged(), bar.interact_pointer_pos()) {
            action = Some(Action::Seek(((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0)));
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(takes: &[Take], recording: Option<f32>, message: &str) -> (String, Option<Action>) {
        let ctx = egui::Context::default();
        let (mut monitor, mut looping) = (String::new(), false);
        let mut action = None;
        let out = ctx.run_ui(egui::RawInput::default(), |u| {
            // Open by default in a fresh context, so the body is drawn.
            action = ui(u, View { takes, recording, playing: None, rendering: None, outputs: &[], monitor: &mut monitor, looping: &mut looping, message });
        });
        let text = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|");
        (text, action)
    }

    #[test]
    fn clocks_read_minutes_and_seconds() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(75.9), "1:15");
    }

    #[test]
    fn an_empty_section_offers_recording() {
        let (text, _) = frame(&[], None, "");
        assert!(text.contains("Record a take") && text.contains("No takes yet."), "{text}");
    }

    #[test]
    fn takes_are_listed_with_their_length_and_the_timer_runs_while_recording() {
        let take = Take { path: "Take 1.wav".into(), name: "Take 1".into(), seconds: 65.0 };
        let (text, _) = frame(&[take], Some(3.0), "Saved.");
        assert!(text.contains("Take 1") && text.contains("1:05") && text.contains("Recording 0:03") && text.contains("Saved."), "{text}");
    }
}
