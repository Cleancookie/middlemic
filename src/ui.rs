//! Simple egui front end: pick devices, start/stop, adjust each stage.

use crate::audio::{self, Devices, Engine};
use crate::dsp;
use crate::eq_panel::EqPanel;
use crate::rack::{Rack, section_body};
use crate::settings::Settings;
use crate::setup::{self, Progress};
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SYSTEM_DEFAULT: &str = "System default";
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// Meters fall at this fraction per redraw.
const METER_DECAY: f32 = 0.85;

pub fn run(settings: Arc<Settings>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("middlemic")
            .with_inner_size([780.0, 860.0])
            .with_min_inner_size([600.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "middlemic",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(settings)))),
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
            if ui.button(label).clicked() {
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

struct App {
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
    in_level: f32,
    out_level: f32,
    save_path: PathBuf,
    saved_text: String,
    last_save: Instant,
}

impl App {
    fn new(settings: Arc<Settings>) -> Self {
        let save_path = crate::settings::settings_path();
        let saved = std::fs::read_to_string(&save_path).unwrap_or_default();
        let extras = settings.apply_text(&saved);

        let devices = audio::list_devices().unwrap_or(Devices {
            inputs: Vec::new(),
            outputs: Vec::new(),
        });
        let input = extras
            .get("device.input")
            .filter(|n| devices.inputs.contains(n))
            .cloned()
            .unwrap_or_else(|| SYSTEM_DEFAULT.to_string());
        let output = extras
            .get("device.output")
            .filter(|n| devices.outputs.contains(n))
            .cloned()
            .or_else(|| devices.outputs.iter().find(|n| n.contains("CABLE Input")).cloned())
            .unwrap_or_default();

        let mut app = Self {
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
            in_level: 0.0,
            out_level: 0.0,
            save_path,
            saved_text: saved,
            last_save: Instant::now(),
        };
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
        let input = (self.input != SYSTEM_DEFAULT).then(|| self.input.clone());
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
        self.in_level = 0.0;
        self.out_level = 0.0;
    }

    fn save_if_changed(&mut self) {
        if self.last_save.elapsed() < SAVE_EVERY {
            return;
        }
        self.last_save = Instant::now();
        let text = self.settings.to_text(&[
            ("device.input", &self.input),
            ("device.output", &self.output),
        ]);
        if text != self.saved_text {
            if let Some(dir) = self.save_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&self.save_path, &text).is_ok() {
                self.saved_text = text;
            }
        }
    }

    fn device_row(&mut self, ui: &mut egui::Ui) {
        let locked = self.running();
        egui::Grid::new("devices").num_columns(2).show(ui, |ui| {
            ui.label("Microphone");
            ui.add_enabled_ui(!locked, |ui| {
                egui::ComboBox::from_id_salt("input")
                    .width(300.0)
                    .selected_text(&self.input)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.input,
                            SYSTEM_DEFAULT.to_string(),
                            SYSTEM_DEFAULT,
                        );
                        for name in &self.devices.inputs {
                            ui.selectable_value(&mut self.input, name.clone(), name);
                        }
                    });
            });
            ui.end_row();

            ui.label("Output");
            ui.add_enabled_ui(!locked, |ui| {
                egui::ComboBox::from_id_salt("output")
                    .width(300.0)
                    .selected_text(if self.output.is_empty() { "Choose…" } else { &self.output })
                    .show_ui(ui, |ui| {
                        for name in &self.devices.outputs {
                            ui.selectable_value(&mut self.output, name.clone(), name);
                        }
                    });
            });
            ui.end_row();
        });
    }

    fn controls_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.running() {
                if ui.button("Stop").clicked() {
                    self.stop();
                }
            } else if ui
                .add_enabled(!self.output.is_empty(), egui::Button::new("Start"))
                .clicked()
            {
                self.start();
            }
            if ui.add_enabled(!self.running(), egui::Button::new("Refresh devices")).clicked() {
                if let Ok(devices) = audio::list_devices() {
                    self.devices = devices;
                }
            }
            if ui.button("Reset all").clicked() {
                self.settings.reset();
            }
            if ui
                .add_enabled(!self.running(), egui::Button::new("Set up virtual cable…"))
                .clicked()
            {
                self.setup = SetupState::Confirm;
            }
        });
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

    fn meters(&mut self, ui: &mut egui::Ui) {
        let meters = &self.settings.meters;
        // -60 dBFS..0 dBFS across the bar.
        let to_frac = |peak: f32| ((20.0 * peak.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        self.in_level = to_frac(meters.take_input()).max(self.in_level * METER_DECAY);
        self.out_level = to_frac(meters.take_output()).max(self.out_level * METER_DECAY);
        egui::Grid::new("meters").num_columns(2).show(ui, |ui| {
            ui.label("In");
            ui.add(egui::ProgressBar::new(self.in_level).desired_width(300.0));
            ui.end_row();
            ui.label("Out");
            ui.add(egui::ProgressBar::new(self.out_level).desired_width(300.0));
            ui.end_row();
        });
        if self.running() {
            ui.weak(format!(
                "DSP load {:.1}% of one core · chain delay about {:.0} ms (plus 50 ms cushion and device buffers) · audio gaps: {}",
                meters.load() * 100.0,
                meters.latency_ms(),
                meters.dropouts()
            ));
        }
    }
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

        egui::CentralPanel::default().show(ui, |ui| {
            self.device_row(ui);
            self.controls_row(ui);
            if !self.status.is_empty() {
                ui.colored_label(egui::Color32::LIGHT_RED, &self.status);
            }
            ui.separator();
            self.meters(ui);
            ui.separator();
            let running = self.running();
            self.eq_panel.update(&self.settings.meters, running);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let deesser_hz = dsp::deesser_frequency_hz(&self.settings.section(dsp::DEESSER_NAME));
                // The rack, in running order. Only the EQ has a custom body.
                let settings = Arc::clone(&self.settings);
                let eq_panel = &mut self.eq_panel;
                let model_download = &mut self.model_download;
                self.rack.show(ui, &settings, |ui, section, reductions| {
                    if section.name == dsp::EQ_NAME {
                        eq_panel.ui(ui, section, deesser_hz);
                    } else if section.name == dsp::DPDFNET.name {
                        model_row(ui, model_download);
                        section_body(ui, section, reductions);
                    } else {
                        section_body(ui, section, reductions);
                    }
                });
            });
        });

        // Meters need ~20 fps while running; otherwise just wake up to save settings.
        let downloading = self
            .model_download
            .as_ref()
            .is_some_and(|p| p.lock().unwrap().outcome.is_none());
        let setting_up = matches!(self.setup, SetupState::Running(_)) || downloading;
        let next = if self.running() {
            50
        } else if setting_up {
            200
        } else {
            1000
        };
        ui.ctx().request_repaint_after(Duration::from_millis(next));
    }
}
