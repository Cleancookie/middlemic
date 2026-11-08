//! The first-run setup: cable, microphone, output. It only draws and returns what the user
//! chose; the app installs the cable, listens to the mics and saves the result.

use crate::setup::CABLE_OUTPUT_NAME;
use crate::theme;
use egui::{RichText, Ui};
use std::collections::HashMap;

pub const WINDOWS_DEFAULT: &str = "Windows default";
/// The recording side of the cable. Using it as the mic would feed middlemic its own output.
pub const CABLE_RECORDING_NAME: &str = "CABLE Output";
const BAR_WIDTH: f32 = 220.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Step {
    Cable,
    Mic,
    Output,
}

pub enum Event {
    Install,
    Finish,
}

/// What the wizard needs to know about this PC. `levels` are 0..1, by device name.
pub struct Env<'a> {
    pub inputs: &'a [String],
    pub outputs: &'a [String],
    pub default_input: Option<&'a str>,
    pub levels: &'a HashMap<String, f32>,
}

impl Env<'_> {
    pub fn cable_present(&self) -> bool {
        crate::setup::cable_present(self.outputs)
    }

    /// Microphones worth offering: everything but the cable's own recording side.
    pub fn mics(&self) -> impl Iterator<Item = &String> {
        self.inputs.iter().filter(|n| !n.contains(CABLE_RECORDING_NAME))
    }

    /// The Windows default recording device, unless that is the cable; else the first real mic.
    fn best_mic(&self) -> String {
        self.default_input
            .filter(|d| self.mics().any(|m| m == d))
            .map(str::to_string)
            .or_else(|| self.mics().next().cloned())
            .unwrap_or_else(|| WINDOWS_DEFAULT.to_string())
    }
}

pub struct Wizard {
    step: Step,
    pub input: String,
    pub output: String,
}

impl Wizard {
    /// Starts at the microphone when the cable is already there.
    pub fn new(env: &Env) -> Self {
        let mut wizard = Self { step: Step::Cable, input: String::new(), output: String::new() };
        if env.cable_present() {
            wizard.go(Step::Mic, env);
        }
        wizard
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// Moving to a step pre-selects its best guess unless the user already picked something there.
    fn go(&mut self, step: Step, env: &Env) {
        self.step = step;
        match step {
            Step::Mic if !self.input_known(env) => self.input = env.best_mic(),
            Step::Output if !env.outputs.contains(&self.output) => {
                if let Some(cable) = env.outputs.iter().find(|n| n.contains(CABLE_OUTPUT_NAME)) {
                    self.output = cable.clone();
                }
            }
            _ => {}
        }
    }

    fn input_known(&self, env: &Env) -> bool {
        self.input == WINDOWS_DEFAULT || env.mics().any(|m| *m == self.input)
    }

    pub fn show(&mut self, ui: &mut Ui, env: &Env) -> Option<Event> {
        let mut event = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(560.0));
            let number = match self.step {
                Step::Cable => 1,
                Step::Mic => 2,
                Step::Output => 3,
            };
            ui.label(RichText::new(format!("Set up middlemic · step {number} of 3")).color(theme::TEXT_DIM));
            ui.add_space(4.0);
            match self.step {
                Step::Cable => self.cable(ui, env, &mut event),
                Step::Mic => self.mic(ui, env),
                Step::Output => self.output(ui, env, &mut event),
            }
        });
        event
    }

    fn cable(&mut self, ui: &mut Ui, env: &Env, event: &mut Option<Event>) {
        ui.heading("A virtual cable");
        ui.label(
            "Discord, OBS and games can only listen to a microphone. middlemic cleans your mic and plays the result \
             into a virtual audio cable (VB-CABLE, free), and those programs listen to the other end of it.",
        );
        ui.add_space(8.0);
        if env.cable_present() {
            ui.colored_label(egui::Color32::LIGHT_GREEN, "VB-CABLE is installed.");
            ui.add_space(8.0);
            if ui.button("Next").clicked() {
                self.go(Step::Mic, env);
            }
            return;
        }
        ui.label("Installing downloads it from vb-audio.com, checks its signature, and asks Windows for permission.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let install = ui.button("Install");
            theme::help_tooltip(install.clone(), "Install", "Downloads VB-CABLE from vb-audio.com and installs it. Windows asks for administrator permission.");
            if install.clicked() {
                *event = Some(Event::Install);
            }
            let skip = ui.button("Skip");
            theme::help_tooltip(skip.clone(), "Skip", "Carry on without it, for example if you use another virtual cable.");
            if skip.clicked() {
                self.go(Step::Mic, env);
            }
        });
    }

    fn mic(&mut self, ui: &mut Ui, env: &Env) {
        ui.heading("Your microphone");
        ui.label("Say something. The bar next to each microphone moves when it hears you, so you can tell which one is yours.");
        ui.add_space(8.0);
        let default_level = env.default_input.and_then(|d| env.levels.get(d)).copied().unwrap_or(0.0);
        self.mic_row(ui, WINDOWS_DEFAULT, default_level);
        for name in env.mics() {
            self.mic_row(ui, name, env.levels.get(name).copied().unwrap_or(0.0));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                self.go(Step::Cable, env);
            }
            if ui.button("Next").clicked() {
                self.go(Step::Output, env);
            }
        });
    }

    fn mic_row(&mut self, ui: &mut Ui, name: &str, level: f32) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.selectable_value(&mut self.input, name.to_string(), name);
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().icon_width + ui.spacing().item_spacing.x);
                theme::level_bar(ui, level, BAR_WIDTH.min(ui.available_width()));
            });
        });
        ui.add_space(4.0);
    }

    fn output(&mut self, ui: &mut Ui, env: &Env, event: &mut Option<Event>) {
        ui.heading("Where middlemic plays");
        ui.label(format!("middlemic plays your cleaned mic into \"{CABLE_OUTPUT_NAME}\"."));
        ui.label(
            RichText::new(format!(
                "In Discord, OBS or a game, pick \"{CABLE_RECORDING_NAME}\" as your microphone."
            ))
            .strong(),
        );
        ui.add_space(8.0);
        for name in env.outputs {
            ui.selectable_value(&mut self.output, name.clone(), name);
        }
        if !self.output.contains(CABLE_OUTPUT_NAME) {
            ui.add_space(4.0);
            ui.colored_label(
                theme::WARN,
                format!("Choose \"{CABLE_OUTPUT_NAME}\" unless you know what you are doing."),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                self.go(Step::Mic, env);
            }
            if ui.add_enabled(!self.output.is_empty(), egui::Button::new("Done")).clicked() {
                *event = Some(Event::Finish);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2, vec2};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn env_with<'a>(inputs: &'a [String], outputs: &'a [String], default: Option<&'a str>, levels: &'a HashMap<String, f32>) -> Env<'a> {
        Env { inputs, outputs, default_input: default, levels }
    }

    #[test]
    fn skips_the_cable_step_when_it_is_installed_else_starts_there() {
        let (inputs, none) = (names(&["Headset"]), HashMap::new());
        let with = names(&["Speakers", "CABLE Input (VB-Audio Virtual Cable)"]);
        let without = names(&["Speakers"]);
        assert_eq!(Wizard::new(&env_with(&inputs, &with, None, &none)).step(), Step::Mic);
        assert_eq!(Wizard::new(&env_with(&inputs, &without, None, &none)).step(), Step::Cable);
    }

    #[test]
    fn best_guess_is_the_windows_default_never_the_cables_recording_side() {
        let (outputs, none) = (names(&["CABLE Input (VB-Audio Virtual Cable)"]), HashMap::new());
        let inputs = names(&["CABLE Output (VB-Audio Virtual Cable)", "Headset", "Webcam"]);
        let default = env_with(&inputs, &outputs, Some("Webcam"), &none);
        assert_eq!(Wizard::new(&default).input, "Webcam");
        let cable_default = env_with(&inputs, &outputs, Some("CABLE Output (VB-Audio Virtual Cable)"), &none);
        assert_eq!(Wizard::new(&cable_default).input, "Headset");
        let nothing = env_with(&[], &outputs, None, &none);
        assert_eq!(Wizard::new(&nothing).input, WINDOWS_DEFAULT);
    }

    #[test]
    fn the_output_step_preselects_the_cable() {
        let none = HashMap::new();
        let inputs = names(&["Headset"]);
        let outputs = names(&["Speakers", "CABLE Input (VB-Audio Virtual Cable)"]);
        let env = env_with(&inputs, &outputs, None, &none);
        let mut wizard = Wizard::new(&env);
        wizard.go(Step::Output, &env);
        assert_eq!(wizard.output, "CABLE Input (VB-Audio Virtual Cable)");
    }

    /// One frame of the wizard in a window `width` points wide; returns the painted text and the widest widget.
    fn frame(wizard: &mut Wizard, env: &Env, width: f32) -> (String, f32) {
        let ctx = egui::Context::default();
        let mut right = 0.0f32;
        let out = ctx.run_ui(
            egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 700.0))), ..Default::default() },
            |ui| {
                wizard.show(ui, env);
                right = ui.min_rect().right();
            },
        );
        let text = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|");
        (text, right)
    }

    #[test]
    fn every_step_says_its_piece_and_fits_420_points() {
        let levels = HashMap::from([("Headset".to_string(), 0.5)]);
        let inputs = names(&["Headset with a rather long name (USB Audio Device)"]);
        let outputs = names(&["Speakers", "CABLE Input (VB-Audio Virtual Cable)"]);
        let env = env_with(&inputs, &outputs, Some("Headset"), &levels);
        let mut wizard = Wizard::new(&env);
        wizard.go(Step::Mic, &env);
        for (step, wanted) in [
            (Step::Mic, vec!["Windows default", "Headset with"]),
            (Step::Output, vec!["CABLE Output", "plays your cleaned mic into \"CABLE Input\""]),
        ] {
            wizard.go(step, &env);
            for width in [420.0, 800.0] {
                let (text, right) = frame(&mut wizard, &env, width);
                wanted.iter().for_each(|w| assert!(text.contains(w), "{step:?} at {width}: {text}"));
                assert!(right <= width, "{step:?} overflows {width}: {right}");
            }
        }
        let bare = names(&["Speakers"]);
        let env = env_with(&inputs, &bare, None, &levels);
        let mut wizard = Wizard::new(&env);
        let (text, right) = frame(&mut wizard, &env, 420.0);
        assert!(text.contains("virtual audio cable") && text.contains("Install") && text.contains("Skip"), "{text}");
        assert!(right <= 420.0);
    }

    #[test]
    fn a_non_cable_output_gets_the_warning() {
        let (none, inputs) = (HashMap::new(), names(&["Headset"]));
        let outputs = names(&["Speakers", "CABLE Input (VB-Audio Virtual Cable)"]);
        let env = env_with(&inputs, &outputs, None, &none);
        let mut wizard = Wizard::new(&env);
        wizard.step = Step::Output;
        let (text, _) = frame(&mut wizard, &env, 600.0);
        assert!(text.contains("unless you know what you are doing"), "{text}");
        wizard.output = outputs[1].clone();
        assert!(!frame(&mut wizard, &env, 600.0).0.contains("unless you know"));
    }
}
