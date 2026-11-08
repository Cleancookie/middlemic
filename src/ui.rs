//! Simple egui front end: pick devices, start/stop, adjust each stage.

use crate::audio::{self, Devices, Engine};
use crate::compressor_panel;
use crate::cpu::CpuMeter;
use crate::dsp;
use crate::eq_panel::EqPanel;
use crate::feedback_panel;
use crate::presets::{self, Presets};
use crate::icons::gear_button;
use crate::level::{self, LevelMeter};
use crate::rack::{Rack, section_body};
use crate::settings::Settings;
use crate::settings_page;
use crate::setup::{self, Progress};
use crate::storage;
use crate::theme;
use crate::wizard::{self, Wizard};
use eframe::egui::{self, vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SAVE_EVERY: Duration = Duration::from_secs(1);
/// Meters fall at this fraction per redraw.
const METER_DECAY: f32 = 0.85;
/// Narrower than this (in points, so DPI and zoom are already counted) the top bar uses two rows.
const WIDE: f32 = 560.0;
const LEVEL_WIDTH: f32 = 90.0;
const LABEL_WIDTH: f32 = 40.0;
const PRESET_WIDTH: f32 = 130.0;

pub fn run(settings: Arc<Settings>, minimized: bool) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("middlemic")
            .with_inner_size([780.0, 860.0])
            .with_min_inner_size([420.0, 360.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
                    .expect("bundled icon is a valid PNG"),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "middlemic",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            if minimized {
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            Ok(Box::new(App::new(settings)))
        }),
    )
}

/// The download button for the DPDFNet model, shown until the file is installed. The running
/// stage looks for the file again every second, so it starts working as soon as it lands.
fn model_row(ui: &mut egui::Ui, download: &mut Option<Arc<Mutex<Progress>>>) {
    let model = &dsp::DPDFNET;
    if dsp::model_installed(model) {
        *download = None;
        return;
    }
    ui.label(format!(
        "The model file ({} MB, Apache-2.0, from huggingface.co/Ceva-IP/DPDFNet) is not installed.",
        model.megabytes
    ));
    let state = download.as_ref().map(|p| {
        let p = p.lock().unwrap();
        (p.message.clone(), p.outcome.clone())
    });
    match state {
        Some((message, None)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(message);
            });
        }
        previous => {
            if let Some((_, Some(Err(why)))) = &previous {
                ui.colored_label(egui::Color32::LIGHT_RED, why);
            }
            let label = if previous.is_some() { "Try again" } else { "Download model" };
            let button = ui.button(label);
            theme::help_tooltip(
                button.clone(),
                label,
                "Downloads the AI model this stage needs (checked against a known checksum before use). The stage starts working as soon as the file lands.",
            );
            if button.clicked() {
                *download = Some(setup::spawn_model_download(
                    model.url,
                    dsp::model_download_dir(),
                    model.file,
                    model.sha256,
                ));
            }
        }
    }
}

/// The one-time virtual cable setup dialog.
enum SetupState {
    Closed,
    Confirm,
    Running(Arc<Mutex<Progress>>),
    Finished(Result<String, String>),
}

/// A name prompt for Save as or Rename.
struct NameDialog {
    renaming: bool,
    name: String,
    error: String,
}

enum PresetDialog {
    Closed,
    Name(NameDialog),
    ConfirmDelete,
}

struct App {
    presets: Presets,
    preset_dialog: PresetDialog,
    setup: SetupState,
    /// The DPDFNet model download, while there is one to show.
    model_download: Option<Arc<Mutex<Progress>>>,
    eq_panel: EqPanel,
    rack: Rack,
    settings: Arc<Settings>,
    devices: Devices,
    input: String,
    output: String,
    engine: Option<Engine>,
    status: String,
    in_meter: LevelMeter,
    out_meter: LevelMeter,
    /// Seconds a full level bar takes to fall; set in settings.
    meter_fall: f32,
    /// Listens to the chosen mic while the engine is stopped, so the Input bar is always live.
    input_monitor: Option<(Option<String>, audio::MicMonitor)>,
    last_levels: Option<Instant>,
    save_path: PathBuf,
    saved_text: String,
    last_save: Instant,
    cpu: CpuMeter,
    page: Page,
    settings_page: settings_page::Page,
    /// Why the last settings action failed, shown at the top of the page.
    page_error: String,
    storage: Vec<storage::Entry>,
    autostart: bool,
    /// Whether the setup wizard has been finished; remembered in settings.ini.
    setup_done: bool,
    wizard: Option<Wizard>,
    /// Listens to every mic while the wizard shows them.
    mic_monitor: Option<audio::MicMonitor>,
    mic_levels: HashMap<String, f32>,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Rack,
    Settings,
}

const SETUP_DONE_KEY: &str = "setup.done";
const METER_FALL_KEY: &str = "meter.fall";

impl App {
    fn new(settings: Arc<Settings>) -> Self {
        let save_path = crate::settings::settings_path();
        let saved = std::fs::read_to_string(&save_path).unwrap_or_default();
        let extras = settings.apply_text(&saved);

        let devices = audio::list_devices().unwrap_or(Devices {
            inputs: Vec::new(),
            outputs: Vec::new(),
            default_input: None,
        });
        let input = extras
            .get("device.input")
            .filter(|n| devices.inputs.contains(n))
            .cloned()
            .unwrap_or_else(|| wizard::WINDOWS_DEFAULT.to_string());
        let output = extras
            .get("device.output")
            .filter(|n| devices.outputs.contains(n))
            .cloned()
            .or_else(|| devices.outputs.iter().find(|n| n.contains("CABLE Input")).cloned())
            .unwrap_or_default();

        let remembered = extras.get("preset").map_or("", String::as_str);
        let presets = Presets::new(presets::presets_dir(), remembered);

        let mut app = Self {
            presets,
            preset_dialog: PresetDialog::Closed,
            setup: SetupState::Closed,
            model_download: None,
            eq_panel: EqPanel::default(),
            rack: Rack::default(),
            settings,
            devices,
            input,
            output,
            engine: None,
            status: String::new(),
            in_meter: LevelMeter::default(),
            out_meter: LevelMeter::default(),
            meter_fall: extras
                .get(METER_FALL_KEY)
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(level::DEFAULT_FALL_SECONDS, |v| v.clamp(*level::FALL_RANGE.start(), *level::FALL_RANGE.end())),
            input_monitor: None,
            last_levels: None,
            save_path,
            saved_text: saved,
            last_save: Instant::now(),
            cpu: CpuMeter::new(),
            page: Page::Rack,
            settings_page: settings_page::Page::default(),
            page_error: String::new(),
            storage: Vec::new(),
            autostart: false,
            setup_done: extras.contains_key(SETUP_DONE_KEY),
            wizard: None,
            mic_monitor: None,
            mic_levels: HashMap::new(),
        };
        if !app.setup_done {
            app.open_wizard();
        }
        if app.output.is_empty() {
            app.status = "Pick an output device (a virtual cable such as CABLE Input).".into();
        }
        app
    }

    fn running(&self) -> bool {
        self.engine.as_ref().is_some_and(Engine::is_running)
    }

    fn start(&mut self) {
        self.stop();
        let input = (self.input != wizard::WINDOWS_DEFAULT).then(|| self.input.clone());
        match Engine::start(input, self.output.clone(), self.settings.clone()) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.status.clear();
            }
            Err(err) => self.status = format!("{err:#}"),
        }
    }

    fn stop(&mut self) {
        self.engine = None; // Drop joins the threads.
        self.out_meter.reset();
        self.settings.clear_stage_latencies();
    }

    fn current_text(&self) -> String {
        let fall = format!("{:.2}", self.meter_fall);
        let mut extras = vec![
            (METER_FALL_KEY, fall.as_str()),
            ("device.input", self.input.as_str()),
            ("device.output", self.output.as_str()),
            ("preset", self.presets.current()),
        ];
        if self.setup_done {
            extras.push((SETUP_DONE_KEY, "1"));
        }
        self.settings.to_text(&extras)
    }

    fn save_if_changed(&mut self) {
        if self.last_save.elapsed() < SAVE_EVERY {
            return;
        }
        self.last_save = Instant::now();
        let text = self.current_text();
        if text != self.saved_text {
            if let Some(dir) = self.save_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&self.save_path, &text).is_ok() {
                self.saved_text = text;
            }
        }
    }

    /// preset … | levels | CPU | gear | start. Below `WIDE` points the readouts wrap to a second row.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let wide = ui.available_width() >= WIDE;
        ui.horizontal(|ui| {
            self.preset_picker(ui);
            if wide {
                ui.separator();
                self.readouts(ui);
            }
        });
        if !wide {
            ui.horizontal(|ui| self.readouts(ui));
        }
    }

    /// The preset list and its "…" menu. Loading works while running: the DSP follows the rack.
    fn preset_picker(&mut self, ui: &mut egui::Ui) {
        let mut chosen = None;
        ui.allocate_ui(vec2(PRESET_WIDTH, ui.spacing().interact_size.y), |ui| {
            let picker = egui::ComboBox::from_id_salt("preset")
                .width(PRESET_WIDTH)
                .truncate()
                .selected_text(self.presets.label(&self.settings))
                .show_ui(ui, |ui| {
                    let current = self.presets.current();
                    for name in presets::BUILTINS {
                        if ui.selectable_label(current == name, format!("{name} (built-in)")).clicked() {
                            chosen = Some(name.to_string());
                        }
                    }
                    if !self.presets.user().is_empty() {
                        ui.separator();
                    }
                    for name in self.presets.user() {
                        if ui.selectable_label(current == name, name).clicked() {
                            chosen = Some(name.clone());
                        }
                    }
                })
                .response;
            theme::help_tooltip(
                picker,
                "Preset",
                "A saved set-up of the whole rack. Pick one to load it; you can keep switching while running. A dot after the name means you have changed it since loading.",
            );
        });
        if let Some(name) = chosen {
            let result = self.presets.load(&self.settings, &name);
            self.report(result);
        }
        self.preset_menu(ui);
    }

    fn preset_menu(&mut self, ui: &mut egui::Ui) {
        let editable = self.presets.is_user();
        let (mut save, mut save_as, mut rename, mut delete) = (false, false, false, false);
        let menu = ui.menu_button("…", |ui| {
            let item = |ui: &mut egui::Ui, enabled: bool, label: &str, help: &str| {
                let response = ui.add_enabled(enabled, egui::Button::new(label));
                theme::help_tooltip(response.clone(), label.trim_end_matches('…'), help);
                response.clicked()
            };
            save = item(ui, editable, "Save", "Writes your current rack settings over the loaded preset. Only your own presets can be overwritten, not the built-in ones.");
            save_as = item(ui, true, "Save as…", "Saves the current rack as a new preset under a name you choose. Use this to keep a copy of a built-in preset you tweaked.");
            rename = item(ui, editable, "Rename…", "Gives the loaded preset a new name. Only your own presets can be renamed.");
            delete = item(ui, editable, "Delete…", "Removes the loaded preset from your list after you confirm. The rack stays as it is.");
            if save || save_as || rename || delete {
                ui.close();
            }
        });
        theme::help_tooltip(menu.response, "Preset actions", "Save, rename or delete presets.");
        let dialog = |renaming, name| PresetDialog::Name(NameDialog { renaming, name, error: String::new() });
        if save {
            let result = self.presets.save(&self.settings);
            self.report(result);
        } else if save_as {
            self.preset_dialog = dialog(false, String::new());
        } else if rename {
            self.preset_dialog = dialog(true, self.presets.current().to_string());
        } else if delete {
            self.preset_dialog = PresetDialog::ConfirmDelete;
        }
    }

    /// Show a failed preset action in the status line.
    fn report(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.status = message;
        }
    }

    fn preset_window(&mut self, ctx: &egui::Context) {
        let mut dialog = std::mem::replace(&mut self.preset_dialog, PresetDialog::Closed);
        let mut keep_open = true;
        match &mut dialog {
            PresetDialog::Closed => return,
            PresetDialog::Name(prompt) => {
                let title = if prompt.renaming { "Rename preset" } else { "Save preset as" };
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        let edit = ui.add(egui::TextEdit::singleline(&mut prompt.name).desired_width(240.0));
                        if ui.memory(|m| m.focused().is_none()) {
                            edit.request_focus();
                        }
                        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if !prompt.error.is_empty() {
                            ui.colored_label(theme::ERROR, &prompt.error);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() || enter {
                                let result = if prompt.renaming {
                                    self.presets.rename(&prompt.name)
                                } else {
                                    self.presets.save_as(&self.settings, &prompt.name)
                                };
                                match result {
                                    Ok(()) => keep_open = false,
                                    Err(message) => prompt.error = message,
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                keep_open = false;
                            }
                        });
                    });
            }
            PresetDialog::ConfirmDelete => {
                egui::Window::new("Delete preset")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!("Delete \"{}\"? The rack stays as it is.", self.presets.current()));
                        ui.horizontal(|ui| {
                            if ui.button("Delete").clicked() {
                                let result = self.presets.delete();
                                self.report(result);
                                keep_open = false;
                            }
                            if ui.button("Cancel").clicked() {
                                keep_open = false;
                            }
                        });
                    });
            }
        }
        if keep_open {
            self.preset_dialog = dialog;
        }
    }

    /// levels | CPU | gear | start, with start pinned to the right edge.
    fn readouts(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let (input, output) = (self.in_meter.level(), self.out_meter.level());
            level_row(
                ui,
                "Input",
                input,
                "Input level",
                "How loud your mic is before processing. Always live, even before you press Start. Aim for it to move well into the bar when you talk without sitting at the very end.",
            );
            level_row(
                ui,
                "Output",
                output,
                "Output level",
                "How loud your voice is after the whole rack, as Discord and games hear it. Only moves while middlemic is started. It should look steadier than the input level.",
            );
        });
        ui.separator();
        let cpu = self.cpu.percent();
        let meters = &self.settings.meters;
        let detail = if self.running() {
            format!(
                "Whole app, as a share of all cores (as Task Manager shows it)\n\
                 DSP {:.1}% of one core\nDelay ~{:.0} ms plus a 50 ms cushion and device buffers\nAudio gaps: {}",
                meters.load() * 100.0,
                meters.latency_ms(),
                meters.dropouts()
            )
        } else {
            "Whole app, as a share of all cores (as Task Manager shows it)".to_string()
        };
        theme::help_tooltip_with_hint(
            ui.label(format!("CPU {cpu:.1}%")),
            "CPU use",
            "How hard your PC is working for middlemic. If it is high or you hear gaps, turn off heavy stages such as the AI noise removal.",
            &detail,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            self.start_stop(ui);
            let gear = gear_button(ui);
            theme::help_tooltip(gear.clone(), "Settings", "Devices, launch at startup, stored files and VB-CABLE.");
            if gear.clicked() {
                self.open_settings();
            }
        });
    }

    fn start_stop(&mut self, ui: &mut egui::Ui) {
        {
            if self.running() {
                let stop = ui.button("Stop");
                if stop.clicked() {
                    self.stop();
                }
                theme::help_tooltip(stop, "Stop", "Stops processing. Discord and games will hear nothing from the cable until you Start again.");
            } else {
                let start = ui.add_enabled(!self.output.is_empty(), egui::Button::new("Start"));
                if start.clicked() {
                    self.start();
                }
                theme::help_tooltip(
                    start,
                    "Start",
                    "Begins cleaning your mic and sending it to the output. Choose an output device first.",
                );
            }
        }
    }

    /// Re-list devices, and point the output at the cable if it is there.
    fn refresh_devices_after_setup(&mut self) {
        if let Ok(devices) = audio::list_devices() {
            if let Some(cable) = devices.outputs.iter().find(|n| n.contains(setup::CABLE_OUTPUT_NAME)) {
                self.output = cable.clone();
                self.status.clear();
            }
            self.devices = devices;
        }
    }

    fn setup_window(&mut self, ctx: &egui::Context) {
        let finished = match &self.setup {
            SetupState::Running(progress) => progress.lock().unwrap().outcome.clone(),
            _ => None,
        };
        if let Some(outcome) = finished {
            self.refresh_devices_after_setup();
            self.setup = SetupState::Finished(outcome);
        }
        if matches!(self.setup, SetupState::Closed) {
            return;
        }

        let already = setup::cable_present(&self.devices.outputs);
        let (mut install, mut close) = (false, false);
        egui::Window::new("Set up virtual cable")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                match &self.setup {
                    SetupState::Confirm => {
                        ui.label(
                            "middlemic sends your cleaned mic to a virtual audio cable, \
                             so Discord and games can pick it as a microphone.",
                        );
                        ui.add_space(6.0);
                        ui.label("This will:");
                        ui.label("  1. download VB-CABLE from vb-audio.com");
                        ui.label("  2. check the installer's digital signature (Vincent Burel, VB-Audio's author)");
                        ui.label("  3. install it. Windows will ask for administrator permission, then to trust the driver.");
                        if already {
                            ui.add_space(6.0);
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "\"CABLE Input\" is already installed. This will install it again.",
                            );
                        }
                        ui.add_space(6.0);
                        ui.weak(
                            "VB-CABLE is donationware by VB-Audio (www.vb-cable.com). \
                             All participations are welcome.",
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            install = ui.button("Install").clicked();
                            close = ui.button("Cancel").clicked();
                        });
                    }
                    SetupState::Running(progress) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(progress.lock().unwrap().message.clone());
                        });
                    }
                    SetupState::Finished(outcome) => {
                        match outcome {
                            Ok(message) => ui.colored_label(egui::Color32::LIGHT_GREEN, message),
                            Err(message) => ui.colored_label(egui::Color32::LIGHT_RED, message),
                        };
                        ui.add_space(6.0);
                        close = ui.button("Close").clicked();
                    }
                    SetupState::Closed => {}
                }
            });

        if install {
            self.setup = SetupState::Running(setup::spawn_install());
        } else if close {
            self.setup = SetupState::Closed;
        }
    }

    fn open_wizard(&mut self) {
        self.stop();
        self.refresh_devices();
        self.wizard = Some(Wizard::new(&self.wizard_env_of_devices()));
    }

    fn wizard_env_of_devices(&self) -> wizard::Env<'_> {
        wizard::Env {
            inputs: &self.devices.inputs,
            outputs: &self.devices.outputs,
            default_input: self.devices.default_input.as_deref(),
            levels: &self.mic_levels,
        }
    }

    fn refresh_devices(&mut self) {
        if let Ok(devices) = audio::list_devices() {
            self.devices = devices;
        }
    }

    fn wizard_view(&mut self, ui: &mut egui::Ui) {
        let Some(mut wizard) = self.wizard.take() else { return };
        let event = wizard.show(ui, &self.wizard_env_of_devices());
        match event {
            Some(wizard::Event::Install) => self.setup = SetupState::Running(setup::spawn_install()),
            Some(wizard::Event::Finish) => {
                self.input = wizard.input;
                self.output = wizard.output;
                self.setup_done = true;
                self.status.clear();
                self.mic_monitor = None;
                return;
            }
            None => {}
        }
        self.wizard = Some(wizard);
    }

    /// Listen to every mic while the wizard is on its microphone step, and only then.
    fn update_mic_levels(&mut self) {
        let listening = self.wizard.as_ref().is_some_and(|w| w.step() == wizard::Step::Mic);
        if !listening {
            self.mic_monitor = None;
            return;
        }
        if self.mic_monitor.is_none() {
            let names: Vec<String> = self.wizard_env_of_devices().mics().cloned().collect();
            let names: Vec<Option<String>> = names.into_iter().map(Some).collect();
            self.mic_monitor = Some(audio::MicMonitor::start(&names));
        }
        let Some(monitor) = &self.mic_monitor else { return };
        for (name, peak) in monitor.take_peaks() {
            let level = self.mic_levels.entry(name).or_default();
            *level = peak_to_fraction(peak).max(*level * METER_DECAY);
        }
    }

    fn open_settings(&mut self) {
        self.page = Page::Settings;
        self.page_error.clear();
        self.autostart = crate::startup::is_enabled();
        self.storage = storage::list(&storage::data_dir());
    }

    fn settings_view(&mut self, ui: &mut egui::Ui) {
        use settings_page::{Action, View};
        let action = self.settings_page.show(
            ui,
            View {
                inputs: &self.devices.inputs,
                outputs: &self.devices.outputs,
                input: &mut self.input,
                output: &mut self.output,
                running: self.engine.as_ref().is_some_and(Engine::is_running),
                autostart: self.autostart,
                meter_fall: &mut self.meter_fall,
                storage: &self.storage,
                error: &self.page_error,
            },
        );
        let Some(action) = action else { return };
        self.page_error.clear();
        let result = match action {
            Action::Back => {
                self.page = Page::Rack;
                Ok(())
            }
            Action::RefreshDevices => {
                self.refresh_devices();
                Ok(())
            }
            Action::InstallCable => {
                self.setup = SetupState::Confirm;
                Ok(())
            }
            Action::RunSetup => {
                self.page = Page::Rack;
                self.open_wizard();
                Ok(())
            }
            Action::SetAutostart(on) => crate::startup::set_enabled(on),
            Action::Delete(path) => self.delete_stored(&path),
            Action::ResetEverything => self.reset_everything(),
        };
        if let Err(why) = result {
            self.page_error = why;
        }
        if self.page == Page::Settings {
            self.autostart = crate::startup::is_enabled();
            self.storage = storage::list(&storage::data_dir());
        }
    }

    /// Delete one stored item. What the app keeps in memory is not written back over the deletion.
    fn delete_stored(&mut self, path: &std::path::Path) -> Result<(), String> {
        storage::delete(path).map_err(|e| format!("Could not delete {}: {e}", path.display()))?;
        let current = self.presets.current().to_string();
        self.presets = Presets::new(presets::presets_dir(), &current);
        self.saved_text = self.current_text();
        Ok(())
    }

    /// Delete everything stored, put every setting back, and run the setup again.
    fn reset_everything(&mut self) -> Result<(), String> {
        self.stop();
        storage::delete_all(&storage::data_dir()).map_err(|e| format!("Could not delete everything: {e}"))?;
        self.settings.apply_text(&Settings::new(dsp::default_sections()).to_text(&[]));
        self.presets = Presets::new(presets::presets_dir(), "");
        self.model_download = None;
        self.setup_done = false;
        self.saved_text = self.current_text();
        self.page = Page::Rack;
        self.open_wizard();
        Ok(())
    }

    /// The Input bar follows the engine when running and its own listener otherwise.
    fn update_levels(&mut self) {
        let now = Instant::now();
        let dt = self.last_levels.replace(now).map_or(0.05, |t| (now - t).as_secs_f32().min(0.25));
        let wanted = self.wizard.is_none() && !self.running();
        let device = (self.input != wizard::WINDOWS_DEFAULT).then(|| self.input.clone());
        if !wanted || self.input_monitor.as_ref().is_some_and(|(d, _)| *d != device) {
            self.input_monitor = None;
        }
        if wanted && self.input_monitor.is_none() {
            let monitor = audio::MicMonitor::start(std::slice::from_ref(&device));
            self.input_monitor = Some((device, monitor));
        }
        let meters = &self.settings.meters;
        let input = match &self.input_monitor {
            Some((_, monitor)) => monitor.take_peaks().iter().map(|(_, p)| *p).fold(0.0, f32::max),
            None => meters.take_input(),
        };
        let output = meters.take_output();
        let fall = self.meter_fall;
        self.in_meter.update(peak_to_fraction(input), dt, fall);
        self.out_meter.update(if self.running() { peak_to_fraction(output) } else { 0.0 }, dt, fall);
    }
}

/// -60 dBFS..0 dBFS across a level bar.
fn level_row(ui: &mut egui::Ui, label: &str, level: f32, title: &str, help: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized([LABEL_WIDTH, 12.0], egui::Label::new(egui::RichText::new(label).small().color(theme::TEXT_DIM)));
        theme::help_tooltip(theme::level_bar(ui, level, LEVEL_WIDTH), title, help);
    });
}

fn peak_to_fraction(peak: f32) -> f32 {
    ((20.0 * peak.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(engine) = &self.engine {
            if !engine.is_running() {
                self.status = engine
                    .error()
                    .unwrap_or_else(|| "Stopped.".to_string());
            }
        }
        self.save_if_changed();
        let ctx = ui.ctx().clone();
        self.setup_window(&ctx);
        self.preset_window(&ctx);

        self.update_levels();
        self.update_mic_levels();
        if self.wizard.is_none() {
            egui::Panel::top("devices").show(ui, |ui| self.top_bar(ui));
        }
        if !self.status.is_empty() {
            egui::Panel::top("status").show(ui, |ui| ui.colored_label(theme::ERROR, &self.status));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.wizard.is_some() {
                self.wizard_view(ui);
                return;
            }
            if self.page == Page::Settings {
                self.settings_view(ui);
                return;
            }
            let running = self.running();
            self.eq_panel.update(&self.settings.meters, running);
            let spectrum: Vec<f32> = if running { self.eq_panel.output_heights().collect() } else { Vec::new() };
            egui::ScrollArea::vertical().show(ui, |ui| {
                // The rack, in running order. EQs, compressors and feedback suppressors have custom bodies.
                let settings = Arc::clone(&self.settings);
                let items = settings.items();
                let eq_panel = &mut self.eq_panel;
                let model_download = &mut self.model_download;
                self.rack.show(ui, &settings, |ui, item, reductions| {
                    let section = &item.section;
                    if item.kind() == dsp::EQ_NAME {
                        eq_panel.ui(ui, section, dsp::deesser_after(&items, item.id));
                    } else if item.kind() == dsp::multiband::NAME {
                        compressor_panel::ui(ui, section, reductions, &spectrum);
                    } else if item.kind() == dsp::feedback::NAME {
                        let position = items.iter().position(|i| i.id == item.id).unwrap_or(0);
                        feedback_panel::ui(ui, section, theme::accent(position, items.len()));
                    } else if item.kind() == dsp::DPDFNET.name {
                        model_row(ui, model_download);
                        section_body(ui, section, reductions);
                    } else {
                        section_body(ui, section, reductions);
                    }
                });
                ui.add_space(8.0);
                let reset_all = ui.small_button("Reset all stages");
                if reset_all.clicked() {
                    self.settings.reset();
                }
                theme::help_tooltip(
                    reset_all,
                    "Reset all stages",
                    "Puts every control in every stage back to its starting value. The stages and their order stay as they are.",
                );
            });
        });

        // Meters need ~20 fps while running; otherwise just wake up to save settings.
        let downloading = self
            .model_download
            .as_ref()
            .is_some_and(|p| p.lock().unwrap().outcome.is_none());
        let setting_up = matches!(self.setup, SetupState::Running(_)) || downloading;
        let next = if self.running() || self.mic_monitor.is_some() || self.input_monitor.is_some() {
            50
        } else if setting_up {
            200
        } else {
            1000
        };
        ui.ctx().request_repaint_after(Duration::from_millis(next));
    }
}
