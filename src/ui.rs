//! Simple egui front end: pick devices, start/stop, adjust each stage.

use crate::audio::{self, Devices, Engine};
use crate::compressor_panel;
use crate::cpu::CpuMeter;
use crate::dsp;
use crate::eq_panel::EqPanel;
use crate::feedback_panel;
use crate::presets::{self, Presets};
use crate::rack::{Rack, cross_button, section_body};
use crate::settings::Settings;
use crate::theme;
use crate::setup::{self, Progress};
use eframe::egui::{self, Vec2, vec2};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SYSTEM_DEFAULT: &str = "System default";
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// Meters fall at this fraction per redraw.
const METER_DECAY: f32 = 0.85;
/// Narrower than this (in points, so DPI and zoom are already counted) the top bar uses two rows.
const WIDE: f32 = 800.0;
const LEVEL_WIDTH: f32 = 90.0;
/// Levels, CPU readout and Start, roughly.
const READOUTS_WIDTH: f32 = 260.0;
const PRESET_WIDTH: f32 = 130.0;
/// The refresh buttons, the preset menu button, separators and gaps between the pickers.
const CONTROLS_WIDTH: f32 = 80.0 + 60.0;

pub fn run(settings: Arc<Settings>) -> eframe::Result {
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
    in_level: f32,
    out_level: f32,
    save_path: PathBuf,
    saved_text: String,
    last_save: Instant,
    cpu: CpuMeter,
    banner_dismissed: bool,
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
            in_level: 0.0,
            out_level: 0.0,
            save_path,
            saved_text: saved,
            last_save: Instant::now(),
            cpu: CpuMeter::new(),
            banner_dismissed: false,
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
            ("preset", self.presets.current()),
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

    /// preset … | input ⟳ | output ⟳ | levels | CPU | start. Below `WIDE` points the readouts wrap to a second row.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let wide = width >= WIDE;
        // Room left for the two pickers after the preset picker, buttons, separators and (when wide) readouts.
        let others = CONTROLS_WIDTH + PRESET_WIDTH + if wide { READOUTS_WIDTH } else { 0.0 };
        let picker = ((width - others) / 2.0).clamp(64.0, 320.0);
        ui.horizontal(|ui| {
            self.preset_picker(ui);
            ui.separator();
            let locked = self.running();
            ui.add_enabled_ui(!locked, |ui| {
                self.input_picker(ui, picker);
                self.refresh_button(ui);
                ui.separator();
                self.output_picker(ui, picker);
                self.refresh_button(ui);
            });
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

    fn input_picker(&mut self, ui: &mut egui::Ui, width: f32) {
        // ComboBox::width is only a minimum; the box truncates to the space it is given.
        ui.allocate_ui(vec2(width, ui.spacing().interact_size.y), |ui| {
            let picker = egui::ComboBox::from_id_salt("input")
                .width(width)
                .truncate()
                .selected_text(&self.input)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.input, SYSTEM_DEFAULT.to_string(), SYSTEM_DEFAULT);
                    for name in &self.devices.inputs {
                        ui.selectable_value(&mut self.input, name.clone(), name);
                    }
                })
                .response;
            theme::help_tooltip_with_hint(
                picker,
                "Input device",
                "The microphone to clean up, usually your headset mic. System default follows whatever Windows uses. Locked while running.",
                &self.input,
            );
        });
    }

    fn output_picker(&mut self, ui: &mut egui::Ui, width: f32) {
        ui.allocate_ui(vec2(width, ui.spacing().interact_size.y), |ui| {
            let picker = egui::ComboBox::from_id_salt("output")
                .width(width)
                .truncate()
                .selected_text(if self.output.is_empty() { "Choose output…" } else { &self.output })
                .show_ui(ui, |ui| {
                    for name in &self.devices.outputs {
                        ui.selectable_value(&mut self.output, name.clone(), name);
                    }
                })
                .response;
            theme::help_tooltip_with_hint(
                picker,
                "Output device",
                "Where the cleaned mic goes. Choose CABLE Input (VB-CABLE), then pick CABLE Output as your microphone in Discord or your game. Locked while running.",
                &self.output,
            );
        });
    }

    fn refresh_button(&mut self, ui: &mut egui::Ui) {
        let refresh = refresh_icon(ui);
        if refresh.clicked() {
            if let Ok(devices) = audio::list_devices() {
                self.devices = devices;
            }
        }
        theme::help_tooltip(refresh, "Refresh devices", "Looks for microphones and outputs again. Use it after plugging in a headset or installing VB-CABLE.");
    }

    /// levels | CPU | start, with start pinned to the right edge.
    fn readouts(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            theme::help_tooltip(
                level_bar(ui, self.in_level),
                "Input level",
                "How loud your mic is before processing. Aim for it to move well into the bar when you talk without sitting at the very end.",
            );
            theme::help_tooltip(
                level_bar(ui, self.out_level),
                "Output level",
                "How loud your voice is after the whole rack, as Discord and games hear it. It should look steadier than the input level.",
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
        });
    }

    /// Shown on every launch until dismissed, while no virtual cable is installed.
    fn cable_banner(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(theme::WARN, "Discord and games need a virtual cable to hear middlemic.");
            let setup = ui.add_enabled(!self.running(), egui::Button::new("Set up VB-CABLE…"));
            if setup.clicked() {
                self.setup = SetupState::Confirm;
            }
            theme::help_tooltip(
                setup,
                "Set up VB-CABLE",
                "Installs VB-CABLE, a free virtual audio cable. middlemic plays your cleaned mic into it, and Discord or your game listens to it as a microphone. Windows asks for permission. Not available while running.",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hide = cross_button(ui);
                if hide.clicked() {
                    self.banner_dismissed = true;
                }
                theme::help_tooltip(hide, "Hide", "Hides this message until you next start the app.");
            });
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

    fn update_levels(&mut self) {
        let meters = &self.settings.meters;
        // -60 dBFS..0 dBFS across the bar.
        let to_frac = |peak: f32| ((20.0 * peak.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        self.in_level = to_frac(meters.take_input()).max(self.in_level * METER_DECAY);
        self.out_level = to_frac(meters.take_output()).max(self.out_level * METER_DECAY);
    }

}

/// A button with a painted circular arrow; egui's default fonts have no refresh glyph.
fn refresh_icon(ui: &mut egui::Ui) -> egui::Response {
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        painter.rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        let (c, r) = (rect.center(), rect.height() * 0.28);
        let stroke = egui::Stroke::new(1.5, visuals.fg_stroke.color);
        let at = |a: f32| c + r * Vec2::angled(a);
        // A 300° arc with a gap at the top right, arrowhead on its clockwise end.
        let (start, end) = (-1.2_f32, 4.0_f32);
        let points: Vec<_> = (0..=24).map(|i| at(start + (end - start) * i as f32 / 24.0)).collect();
        painter.add(egui::Shape::line(points, stroke));
        let tip = at(start);
        let tangent = Vec2::angled(start - std::f32::consts::FRAC_PI_2);
        let side = Vec2::angled(start);
        let head = vec![tip, tip - tangent * 3.5 + side * 3.0, tip - tangent * 3.5 - side * 3.0];
        painter.add(egui::Shape::convex_polygon(head, visuals.fg_stroke.color, egui::Stroke::NONE));
    }
    response
}

/// A thin level meter with a visible track, so it reads as a meter even in silence.
fn level_bar(ui: &mut egui::Ui, level: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(LEVEL_WIDTH, 5.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::GRID);
    if level > 0.0 {
        let lit = egui::Rect::from_min_size(rect.min, vec2(rect.width() * level.clamp(0.0, 1.0), rect.height()));
        painter.rect_filled(lit, 2.0, theme::accent(3, 4));
    }
    response
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
        egui::Panel::top("devices").show(ui, |ui| self.top_bar(ui));
        if !self.banner_dismissed && !setup::cable_present(&self.devices.outputs) {
            egui::Panel::top("cable_banner").show(ui, |ui| self.cable_banner(ui));
        }
        if !self.status.is_empty() {
            egui::Panel::top("status").show(ui, |ui| ui.colored_label(theme::ERROR, &self.status));
        }
        egui::CentralPanel::default().show(ui, |ui| {
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
