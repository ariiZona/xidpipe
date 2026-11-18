// SPDX-License-Identifier: GPL-3.0-or-later
//! The window: pick, edit and save presets; see live input, output and rumble per controller.
//! Every change applies immediately; the network loop runs on its own thread and never waits on the UI.

use crate::engine::{self, PadView, Settings, Status};
use crate::output;
use crate::pad::{self, Backend, Preset, Profile};
use eframe::egui::{self, Color32, RichText, Sense, Stroke, Ui, vec2};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OUT_BUTTONS: [(u16, &str); 15] = [
    (0x1000, "A"),
    (0x2000, "B"),
    (0x4000, "X"),
    (0x8000, "Y"),
    (0x0100, "LB"),
    (0x0200, "RB"),
    (0x0020, "Back"),
    (0x0010, "Start"),
    (0x0040, "LS"),
    (0x0080, "RS"),
    (0x0001, "Up"),
    (0x0002, "Down"),
    (0x0004, "Left"),
    (0x0008, "Right"),
    (0x0400, "Guide"),
];
const IN_DIGITAL: [(u8, &str); 8] = [
    (0x01, "Up"),
    (0x02, "Down"),
    (0x04, "Left"),
    (0x08, "Right"),
    (0x10, "Start"),
    (0x20, "Back"),
    (0x40, "LS"),
    (0x80, "RS"),
];
const IN_ANALOG: [&str; 8] = ["A", "B", "X", "Y", "Black", "White", "LT", "RT"];
const ERROR: Color32 = Color32::from_rgb(0xd0, 0x40, 0x40);

pub fn run() -> Result<(), String> {
    let name = "xbox".to_string();
    let preset = crate::load_preset(&name)?;
    let profile = Profile::new(&preset)?;
    let status = Arc::new(Mutex::new(Status::default()));
    let (tx, rx) = mpsc::channel();
    let st = status.clone();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let result = engine::run(
                Settings {
                    profile,
                    xbox: None,
                },
                rx,
                Some(st.clone()),
                false,
            );
            if let Err(e) = result {
                st.lock().unwrap().error = Some(e);
            }
        })
        .map_err(|e| e.to_string())?;

    let app = App {
        applied: (preset.clone(), String::new()),
        name,
        preset,
        xbox_ip: String::new(),
        save_as: String::new(),
        note: None,
        invalid: None,
        tx,
        status,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("xidpipe")
            .with_inner_size([960.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native("xidpipe", options, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| e.to_string())
}

struct App {
    name: String,
    preset: Preset,
    /// What the engine currently runs, to notice edits.
    applied: (Preset, String),
    xbox_ip: String,
    save_as: String,
    /// Result of the last load/save: (text, is error)
    note: Option<(String, bool)>,
    /// Why the current edit can't be applied
    invalid: Option<String>,
    tx: Sender<Settings>,
    status: Arc<Mutex<Status>>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        // Live view: ~60 fps while open. Input latency doesn't depend on this.
        ui.ctx().request_repaint_after(Duration::from_millis(16));
        self.apply();
        egui::Panel::left("settings")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_ui(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.live_ui(ui));
        });
    }
}

impl App {
    /// Send the edited preset to the engine whenever it changed and is valid.
    fn apply(&mut self) {
        let current = (self.preset.clone(), self.xbox_ip.clone());
        if current == self.applied {
            return;
        }
        self.applied = current;
        let xbox = match self.xbox_ip.trim() {
            "" => Ok(None),
            ip => crate::parse_ip(ip).map(Some),
        };
        match (Profile::new(&self.preset), xbox) {
            (Ok(profile), Ok(xbox)) => {
                self.invalid = None;
                let _ = self.tx.send(Settings { profile, xbox });
            }
            (Err(e), _) | (_, Err(e)) => self.invalid = Some(e),
        }
    }

    fn load(&mut self, name: &str) {
        match crate::load_preset(name) {
            Ok(p) => {
                self.preset = p;
                self.name = name.to_string();
                self.note = None;
            }
            Err(e) => self.note = Some((e, true)),
        }
    }

    fn save(&mut self, name: String, overwrite: bool) {
        let text = toml::to_string(&self.preset).map_err(|e| e.to_string()).and_then(|body| {
            let text = format!(
                "# xidpipe preset, saved from the window. pc/presets/xbox.toml in the repo explains each setting.\n{body}"
            );
            crate::write_preset(&name, &text, overwrite)
        });
        self.note = Some(match text {
            Ok(file) => {
                self.name = name;
                self.save_as.clear();
                (format!("Saved {}", file.display()), false)
            }
            Err(e) => (e, true),
        });
    }

    fn settings_ui(&mut self, ui: &mut Ui) {
        ui.heading("Preset");
        ui.horizontal(|ui| {
            let mut pick = None;
            egui::ComboBox::from_id_salt("preset")
                .selected_text(&self.name)
                .show_ui(ui, |ui| {
                    let names = crate::BUILTIN
                        .iter()
                        .map(|b| b.0.to_string())
                        .chain(crate::saved_presets());
                    for n in names {
                        if ui.selectable_label(n == self.name, &n).clicked() {
                            pick = Some(n);
                        }
                    }
                });
            if let Some(n) = pick {
                self.load(&n);
            }
            let builtin = crate::is_builtin(&self.name);
            let save = ui
                .add_enabled(!builtin, egui::Button::new("Save"))
                .on_disabled_hover_text("Built-in presets can't be changed. Use \"Save as\".");
            if save.clicked() {
                self.save(self.name.clone(), true);
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.save_as)
                    .hint_text("new preset name")
                    .desired_width(160.0),
            );
            if ui.button("Save as").clicked() {
                self.save(self.save_as.trim().to_string(), false);
            }
        });
        if let Some((text, error)) = &self.note {
            ui.label(if *error {
                RichText::new(text).color(ERROR)
            } else {
                RichText::new(text)
            });
        }

        ui.separator();
        ui.heading("Controller type");
        let available = output::available();
        egui::ComboBox::from_id_salt("backend")
            .selected_text(backend_label(self.preset.backend))
            .show_ui(ui, |ui| {
                for b in &available {
                    ui.selectable_value(&mut self.preset.backend, *b, backend_label(*b));
                }
            });
        if !available.contains(&self.preset.backend) {
            ui.colored_label(ERROR, "Not available on this OS. Pick another type.");
        }
        if self.preset.backend == Backend::Vjoy {
            ui.add(
                egui::Slider::new(&mut self.preset.vjoy_device, 1..=16)
                    .text("vJoy device for pad 1"),
            );
            ui.label(
                RichText::new(
                    "One vJoy device per pad (pad 2 uses the next one). Its 8 axes: sticks, \
                     LT/RT on Z/Rz, sl0/sl1 on Slider/Dial. To give pressure buttons their own \
                     axes, map them to lt, rt, sl0 or sl1 below.",
                )
                .small(),
            );
        }
        ui.checkbox(&mut self.preset.pressure, "Pressure-sensitive face buttons");
        ui.label(
            RichText::new(
                "A B X Y Black White send how hard they're pressed when mapped to an analog \
                 output (lt, rt, and on vJoy also sl0, sl1).",
            )
            .small(),
        );

        ui.separator();
        ui.heading("Deadzones");
        ui.horizontal(|ui| {
            if ui.button("Xbox defaults").clicked() {
                let d = Preset::default();
                self.set_deadzones(
                    d.left_stick_deadzone,
                    d.right_stick_deadzone,
                    d.analog_deadzone,
                );
            }
            if ui.button("Zero deadzone").clicked() {
                self.set_deadzones(0, 0, 0);
            }
        });
        ui.add(
            egui::Slider::new(&mut self.preset.left_stick_deadzone, 0..=32000).text("left stick"),
        );
        ui.add(
            egui::Slider::new(&mut self.preset.right_stick_deadzone, 0..=32000).text("right stick"),
        );
        ui.add(egui::Slider::new(&mut self.preset.analog_deadzone, 0..=255).text("analog buttons"));
        ui.add(egui::Slider::new(&mut self.preset.press_threshold, 0..=255).text("press point"))
            .on_hover_text(
                "Pressure at which an analog button counts as pressed for a digital output.",
            );

        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Mapping");
            if ui.button("Reset").clicked() {
                self.preset.map.clear();
            }
        });
        ui.label(RichText::new("PC output ← controller input").small());
        egui::Grid::new("map")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (name, axis, default) in pad::outputs() {
                    if name.starts_with("sl") {
                        ui.label(name)
                            .on_hover_text("Extra axis, vJoy only (Slider / Dial)");
                    } else {
                        ui.label(name);
                    }
                    let current = self
                        .preset
                        .map
                        .get(name)
                        .map_or(default, String::as_str)
                        .to_string();
                    let mut pick = current.clone();
                    let inputs: &[&str] = if axis {
                        &pad::AXIS_INPUTS
                    } else {
                        &pad::BUTTON_INPUTS
                    };
                    egui::ComboBox::from_id_salt(("map", name))
                        .selected_text(&pick)
                        .show_ui(ui, |ui| {
                            for i in inputs {
                                ui.selectable_value(&mut pick, i.to_string(), *i);
                            }
                        });
                    if pick != current {
                        if pick == default {
                            self.preset.map.remove(name);
                        } else {
                            self.preset.map.insert(name.to_string(), pick);
                        }
                    }
                    ui.end_row();
                }
            });

        ui.separator();
        ui.heading("Xbox address");
        ui.add(
            egui::TextEdit::singleline(&mut self.xbox_ip)
                .hint_text("automatic (broadcast)")
                .desired_width(160.0),
        )
        .on_hover_text(
            "Only needed if the Xbox isn't found: type the IP shown on the Xbox screen.",
        );

        if let Some(e) = &self.invalid {
            ui.separator();
            ui.colored_label(ERROR, format!("Not applied: {e}"));
        }
    }

    fn set_deadzones(&mut self, left: u16, right: u16, analog: u8) {
        self.preset.left_stick_deadzone = left;
        self.preset.right_stick_deadzone = right;
        self.preset.analog_deadzone = analog;
    }

    fn live_ui(&mut self, ui: &mut Ui) {
        let (xbox, error, pads) = {
            let s = self.status.lock().unwrap();
            (s.xbox, s.error.clone(), s.pads.clone())
        };
        ui.heading("Controllers");
        match xbox {
            Some(a) => ui.label(format!("Xbox connected: {a}")),
            None => ui.label("Looking for the Xbox... Start xidpipe on it. If it isn't found, enter its IP on the left."),
        };
        if let Some(e) = error {
            ui.colored_label(ERROR, e);
        }
        if pads.is_empty() {
            ui.label("No controller yet.");
        }
        for (id, v) in &pads {
            egui::Frame::group(ui.style()).show(ui, |ui| self.pad_ui(ui, *id, v));
        }
    }

    fn pad_ui(&self, ui: &mut Ui, id: u8, v: &PadView) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("Pad {id}"));
            ui.label(format!("USB {:04x}:{:04x}", v.vid, v.pid));
            ui.label(if v.linked { "live" } else { "no data" });
            ui.label(format!("lost packets: {}", v.lost));
            let pct = |m: u16| m as u32 * 100 / 65535;
            ui.label(format!(
                "rumble: {}% / {}%",
                pct(v.rumble.0),
                pct(v.rumble.1)
            ));
        });
        if let Some(e) = &v.error {
            ui.colored_label(ERROR, e);
        }
        let (r, o) = (v.raw.sticks, v.out.sticks);
        ui.horizontal(|ui| {
            stick_plot(
                ui,
                "Left stick",
                (r[0], r[1]),
                (o[0], o[1]),
                self.preset.left_stick_deadzone,
            );
            stick_plot(
                ui,
                "Right stick",
                (r[2], r[3]),
                (o[2], o[3]),
                self.preset.right_stick_deadzone,
            );
            ui.vertical(|ui| {
                for (i, name) in IN_ANALOG.iter().enumerate() {
                    let val = v.raw.analog[i];
                    ui.horizontal(|ui| {
                        ui.add_sized([44.0, 16.0], egui::Label::new(*name));
                        ui.add(
                            egui::ProgressBar::new(val as f32 / 255.0)
                                .desired_width(150.0)
                                .text(val.to_string()),
                        );
                    });
                }
            });
        });
        let pressed_in: Vec<&str> = IN_DIGITAL
            .iter()
            .filter(|b| v.raw.digital & b.0 != 0)
            .map(|b| b.1)
            .collect();
        let pressed_out: Vec<&str> = OUT_BUTTONS
            .iter()
            .filter(|b| v.out.buttons & b.0 != 0)
            .map(|b| b.1)
            .collect();
        ui.label(format!("Digital in: {}", pressed_in.join(" ")));
        ui.label(format!(
            "PC sees: {}  LT {}  RT {}",
            pressed_out.join(" "),
            v.out.lt,
            v.out.rt
        ));
    }
}

fn backend_label(b: Backend) -> &'static str {
    match b {
        Backend::Xinput => "XInput (Xbox 360 controller)",
        Backend::Dinput if cfg!(windows) => "DirectInput (DualShock 4)",
        Backend::Dinput => "Generic gamepad (not XInput)",
        Backend::Vjoy => "vJoy (DirectInput, extra axes)",
        Backend::None => "None (view only)",
    }
}

/// Stick position: grey dot = raw, accent dot = what the PC gets, shaded disc = deadzone.
fn stick_plot(ui: &mut Ui, label: &str, raw: (i16, i16), out: (i16, i16), deadzone: u16) {
    ui.vertical(|ui| {
        ui.label(label);
        let size = 120.0;
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        let p = ui.painter_at(rect);
        let vis = ui.visuals();
        let (c, r) = (rect.center(), size / 2.0 - 5.0);
        p.circle_filled(c, r * deadzone as f32 / 32767.0, vis.faint_bg_color);
        p.circle_stroke(c, r, Stroke::new(1.0, vis.weak_text_color()));
        let at = |v: (i16, i16)| c + vec2(v.0 as f32 / 32767.0 * r, -(v.1 as f32) / 32767.0 * r);
        p.circle_filled(at(raw), 4.0, vis.weak_text_color());
        p.circle_filled(at(out), 4.0, vis.selection.bg_fill);
        ui.label(
            RichText::new(format!(
                "raw {:6} {:6}\nout {:6} {:6}",
                raw.0, raw.1, out.0, out.1
            ))
            .monospace()
            .small(),
        );
    });
}
