//! The settings page: devices, launch at startup, what is stored on disk, and VB-CABLE.
//! It replaces the rack while open. It only draws and returns what the user asked for.

use crate::icons::refresh_button;
use crate::storage::{self, Entry};
use crate::theme;
use egui::{ComboBox, RichText, Ui};
use std::path::PathBuf;

pub enum Action {
    Back,
    RefreshDevices,
    InstallCable,
    RunSetup,
    SetAutostart(bool),
    Delete(PathBuf),
    ResetEverything,
}

pub struct View<'a> {
    pub inputs: &'a [String],
    pub outputs: &'a [String],
    pub input: &'a mut String,
    pub output: &'a mut String,
    /// The pickers and the installer are locked while middlemic is running.
    pub running: bool,
    pub autostart: bool,
    pub storage: &'a [Entry],
    /// The outcome of the last action, if it failed.
    pub error: &'a str,
}

#[derive(Default)]
pub struct Page {
    confirm_reset: bool,
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).heading());
    ui.add_space(2.0);
}

fn picker(ui: &mut Ui, id: &str, label: &str, value: &mut String, choices: &[String], windows_default: bool, help: &str) {
    ui.label(label);
    let shown = if value.is_empty() { "Choose…" } else { value.as_str() };
    let response = ComboBox::from_id_salt(id)
        .width(ui.available_width().clamp(120.0, 360.0))
        .truncate()
        .selected_text(shown.to_string())
        .show_ui(ui, |ui| {
            if windows_default {
                ui.selectable_value(value, crate::wizard::WINDOWS_DEFAULT.to_string(), crate::wizard::WINDOWS_DEFAULT);
            }
            for name in choices {
                ui.selectable_value(value, name.clone(), name);
            }
        })
        .response;
    theme::help_tooltip(response, label, help);
}

impl Page {
    pub fn show(&mut self, ui: &mut Ui, view: View) -> Option<Action> {
        let mut action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(560.0));
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    action = Some(Action::Back);
                }
                ui.label(RichText::new("Settings").heading());
            });
            if !view.error.is_empty() {
                ui.colored_label(theme::ERROR, view.error);
            }

            section(ui, "Devices");
            ui.add_enabled_ui(!view.running, |ui| {
                picker(
                    ui,
                    "input",
                    "Microphone",
                    view.input,
                    view.inputs,
                    true,
                    "The microphone to clean up, usually your headset mic. Windows default follows whatever Windows uses. Locked while running.",
                );
                picker(
                    ui,
                    "output",
                    "Output",
                    view.output,
                    view.outputs,
                    false,
                    "Where the cleaned mic goes. Choose CABLE Input (VB-CABLE), then pick CABLE Output as your microphone in Discord or your game. Locked while running.",
                );
                ui.add_space(4.0);
                let refresh = refresh_button(ui);
                theme::help_tooltip(refresh.clone(), "Refresh devices", "Looks for microphones and outputs again. Use it after plugging in a headset or installing VB-CABLE.");
                if refresh.clicked() {
                    action = Some(Action::RefreshDevices);
                }
            });

            section(ui, "Windows");
            let mut autostart = view.autostart;
            let launch = ui.checkbox(&mut autostart, "Launch when Windows starts");
            theme::help_tooltip(launch, "Launch when Windows starts", "Starts middlemic minimised each time you sign in to Windows. No administrator permission needed. Untick it to stop.");
            if autostart != view.autostart {
                action = Some(Action::SetAutostart(autostart));
            }

            section(ui, "Storage");
            ui.label(RichText::new(format!("Kept in {}", storage::data_dir().display())).color(theme::TEXT_DIM));
            if view.storage.is_empty() {
                ui.label("Nothing is stored yet.");
            }
            for entry in view.storage {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} · {}", entry.label, storage::human_size(entry.bytes)));
                    let delete = ui.small_button("Delete");
                    theme::help_tooltip(delete.clone(), "Delete", "Removes this from disk. Presets and settings cannot be brought back; the AI model can be downloaded again.");
                    if delete.clicked() {
                        action = Some(Action::Delete(entry.path.clone()));
                    }
                });
            }
            ui.add_space(6.0);
            if self.confirm_reset {
                ui.colored_label(theme::WARN, "Delete the models, your presets and all settings, and set middlemic up again?");
                ui.horizontal(|ui| {
                    if ui.button("Yes, reset everything").clicked() {
                        self.confirm_reset = false;
                        action = Some(Action::ResetEverything);
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_reset = false;
                    }
                });
            } else {
                let reset = ui.button("Reset everything…");
                theme::help_tooltip(reset.clone(), "Reset everything", "Deletes everything middlemic stores and puts every setting back to how it was on first launch. You are asked to confirm.");
                self.confirm_reset |= reset.clicked();
            }

            section(ui, "VB-CABLE");
            ui.horizontal_wrapped(|ui| {
                let install = ui.add_enabled(!view.running, egui::Button::new("Install / reinstall VB-CABLE…"));
                theme::help_tooltip(install.clone(), "Install VB-CABLE", "Installs VB-CABLE, a free virtual audio cable. middlemic plays your cleaned mic into it and Discord or your game listens to it as a microphone. Windows asks for permission. Not available while running.");
                if install.clicked() {
                    action = Some(Action::InstallCable);
                }
                let again = ui.add_enabled(!view.running, egui::Button::new("Run setup again"));
                theme::help_tooltip(again.clone(), "Run setup again", "Walks through the first-run setup: cable, microphone and output. Not available while running.");
                if again.clicked() {
                    action = Some(Action::RunSetup);
                }
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

    fn show(page: &mut Page, entries: &[Entry], width: f32) -> (String, f32) {
        let (inputs, outputs) = (vec!["Headset".to_string()], vec!["CABLE Input (VB-Audio Virtual Cable)".to_string()]);
        let (mut input, mut output) = (String::new(), String::new());
        let ctx = egui::Context::default();
        let mut right = 0.0f32;
        let out = ctx.run_ui(
            egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 900.0))), ..Default::default() },
            |ui| {
                page.show(
                    ui,
                    View { inputs: &inputs, outputs: &outputs, input: &mut input, output: &mut output, running: false, autostart: false, storage: entries, error: "" },
                );
                right = ui.min_rect().right();
            },
        );
        let text: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        (text.join("|"), right)
    }

    #[test]
    fn lists_every_section_and_each_stored_item_with_its_size_at_420_points() {
        let entries = [
            Entry { label: "AI models", path: "models".into(), bytes: 10 << 20 },
            Entry { label: "Settings", path: "settings.ini".into(), bytes: 900 },
        ];
        for width in [420.0, 800.0] {
            let (text, right) = show(&mut Page::default(), &entries, width);
            for wanted in ["Devices", "Launch when Windows starts", "AI models · 10.0 MB", "Settings · 900 B", "Reset everything", "Install / reinstall", "Run setup again"] {
                assert!(text.contains(wanted), "{wanted} missing at {width}: {text}");
            }
            assert!(right <= width, "overflows {width}: {right}");
        }
    }

    #[test]
    fn reset_everything_asks_first() {
        let mut page = Page { confirm_reset: true };
        let (text, _) = show(&mut page, &[], 420.0);
        assert!(text.contains("Yes, reset everything") && text.contains("Nothing is stored yet"), "{text}");
    }
}
