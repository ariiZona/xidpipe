// SPDX-License-Identifier: GPL-3.0-or-later
//! The window: pick, edit and save presets; see live input, output and rumble per controller.
//! Every change applies immediately; the network loop runs on its own thread and never waits on the UI.

use crate::engine::{self, Command, PadView, Settings, Status};
use crate::output;
use crate::pad::{self, Backend, Preset, Profile, Xid};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2,
};
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
                    poll_hz: 0,
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
        applied: (preset.clone(), String::new(), 0),
        poll_hz: 0,
        tab: Tab::Setup,
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
    applied: (Preset, String, u16),
    poll_hz: u16,
    tab: Tab,
    xbox_ip: String,
    save_as: String,
    /// Result of the last load/save: (text, is error)
    note: Option<(String, bool)>,
    /// Why the current edit can't be applied
    invalid: Option<String>,
    tx: Sender<Command>,
    status: Arc<Mutex<Status>>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        // Live view: ~60 fps while open. Input latency doesn't depend on this.
        ui.ctx().request_repaint_after(Duration::from_millis(16));
        self.apply();
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::Setup, "Setup"),
                    (Tab::Tester, "Button tester"),
                    (Tab::Debug, "Debug"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
                ui.separator();
                match self.status.lock().unwrap().xbox {
                    Some(a) => ui.label(format!("Xbox: {a}")),
                    None => ui.label("Looking for the Xbox..."),
                };
            });
        });
        match self.tab {
            Tab::Setup => {
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
            Tab::Tester => {
                egui::CentralPanel::default().show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.tester_ui(ui));
                });
            }
            Tab::Debug => {
                egui::CentralPanel::default().show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.debug_ui(ui));
                });
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Setup,
    Tester,
    Debug,
}

impl App {
    /// Send the edited preset to the engine whenever it changed and is valid.
    fn apply(&mut self) {
        let current = (self.preset.clone(), self.xbox_ip.clone(), self.poll_hz);
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
                let poll_hz = self.poll_hz;
                let _ = self.tx.send(Command::Settings(Settings {
                    profile,
                    xbox,
                    poll_hz,
                }));
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
        ui.add(
            egui::Slider::new(&mut self.preset.rumble_strength, 0..=100)
                .text("vibration strength")
                .suffix("%"),
        )
        .on_hover_text(
            "Scales both rumble motors, so it is also the maximum: 50% never shakes harder \
             than half power. 0% turns rumble off. Try it with the Debug tab's test buttons.",
        );
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
        ui.horizontal(|ui| {
            ui.label("Controller polling");
            egui::ComboBox::from_id_salt("poll")
                .selected_text(poll_label(self.poll_hz))
                .show_ui(ui, |ui| {
                    for hz in POLL_RATES {
                        ui.selectable_value(&mut self.poll_hz, hz, poll_label(hz));
                    }
                });
        })
        .response
        .on_hover_text(
            "How often the Xbox asks the controller for its state. Faster lowers input delay; \
             almost every controller handles 1000 Hz, but pick a lower rate if one misbehaves.",
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
            ui.vertical(|ui| analog_bars(ui, &v.raw));
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

    /// Button tester: the raw controller as the Xbox reads it, before deadzones and mapping.
    fn tester_ui(&self, ui: &mut Ui) {
        let pads = self.status.lock().unwrap().pads.clone();
        ui.heading("Button tester");
        ui.label(
            "Raw controller input as the Xbox reads it, before deadzones and mapping. \
             A B X Y, Black, White and the triggers fill up with how hard they're pressed.",
        );
        if pads.is_empty() {
            ui.label("No controller yet. Plug one into the Xbox.");
        }
        for (id, v) in &pads {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(format!(
                    "Pad {id}{}",
                    if v.linked { "" } else { " (no data)" }
                ));
                ui.horizontal(|ui| {
                    pad_drawing(ui, &v.raw);
                    ui.vertical(|ui| {
                        analog_bars(ui, &v.raw);
                        let s = v.raw.sticks;
                        ui.label(
                            RichText::new(format!(
                                "left stick  {:6} {:6}\nright stick {:6} {:6}",
                                s[0], s[1], s[2], s[3]
                            ))
                            .monospace(),
                        );
                        let pressed: Vec<&str> = IN_DIGITAL
                            .iter()
                            .filter(|b| v.raw.digital & b.0 != 0)
                            .map(|b| b.1)
                            .collect();
                        ui.label(format!("Pressed: {}", pressed.join(" ")));
                    });
                });
            });
        }
    }

    fn debug_ui(&mut self, ui: &mut Ui) {
        let (xbox, rtt, pads, log, error) = {
            let s = self.status.lock().unwrap();
            (
                s.xbox,
                s.rtt,
                s.pads.clone(),
                s.log.clone(),
                s.error.clone(),
            )
        };
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        ui.heading("Connection");
        egui::Grid::new("connection")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("Xbox");
                ui.label(xbox.map_or("not found yet".into(), |a| a.to_string()));
                ui.end_row();
                ui.label("Network round trip");
                ui.label(rtt.map_or("-".into(), |(now, best, avg)| {
                    format!(
                        "{:.2} ms now, {:.2} ms best, {:.2} ms average",
                        ms(now),
                        ms(best),
                        ms(avg)
                    )
                }));
                ui.end_row();
                ui.label("Polling asked of the Xbox");
                ui.label(poll_label(self.poll_hz));
                ui.end_row();
            });
        if let Some(e) = error {
            ui.colored_label(ERROR, e);
        }

        ui.separator();
        ui.heading("Controllers");
        if pads.is_empty() {
            ui.label("No controller yet.");
        }
        for (&id, v) in &pads {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(format!("Pad {id}"));
                egui::Grid::new(("debug", id))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        let mut row = |k: &str, val: String| {
                            ui.label(k);
                            ui.label(val);
                            ui.end_row();
                        };
                        row("USB id", format!("{:04x}:{:04x}", v.vid, v.pid));
                        if let Some(i) = v.info {
                            row("XID type", xid_label(i.xid));
                            row(
                                "Polling",
                                format!(
                                    "controller asks every {} ms, polled every {} ms ({} Hz)",
                                    i.dev_interval,
                                    i.interval,
                                    1000 / i.interval.max(1) as u32
                                ),
                            );
                            row("USB reports/s (Xbox)", i.reports_per_s.to_string());
                            row("USB errors", i.usb_errors.to_string());
                            row(
                                "Rumble motors",
                                if i.has_rumble {
                                    "yes"
                                } else {
                                    "no OUT endpoint"
                                }
                                .into(),
                            );
                        } else {
                            row("Xbox details", "waiting (sent once a second)".into());
                        }
                        row(
                            "Packets/s received (PC)",
                            format!("{:.0}, longest gap {:.1} ms", v.rate, ms(v.worst_gap)),
                        );
                        row("Lost packets", v.lost.to_string());
                        row(
                            "Last packet",
                            v.last_rx
                                .map_or("-".into(), |t| format!("{:.0} ms ago", ms(t.elapsed()))),
                        );
                        let pct = |m: u16| m as u32 * 100 / 65535;
                        row(
                            "Rumble now",
                            format!("left {}%, right {}%", pct(v.rumble.0), pct(v.rumble.1)),
                        );
                        row(
                            "Raw report",
                            v.report
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<Vec<_>>()
                                .join(" "),
                        );
                        if let Some(e) = &v.error {
                            row("Backend error", e.clone());
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label("Test rumble:");
                    let half = 0x8000;
                    let tests = [
                        ("Left motor", (half, 0)),
                        ("Right motor", (0, half)),
                        ("Both, full", (0xffff, 0xffff)),
                        ("Stop", (0, 0)),
                    ];
                    for (label, level) in tests {
                        if ui.button(label).clicked() {
                            let time = Duration::from_millis(if level == (0, 0) { 0 } else { 700 });
                            let _ = self.tx.send(Command::TestRumble {
                                pad: id,
                                level,
                                time,
                            });
                        }
                    }
                });
            });
        }

        ui.separator();
        ui.heading("Log");
        egui::ScrollArea::vertical()
            .id_salt("log")
            .max_height(220.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &log {
                    ui.label(RichText::new(line).monospace().small());
                }
            });
    }
}

const POLL_RATES: [u16; 5] = [0, 125, 250, 500, 1000];

fn poll_label(hz: u16) -> String {
    match hz {
        0 => "Auto (controller's own rate, at least 125 Hz)".into(),
        hz => format!("{hz} Hz (every {} ms)", engine::poll_ms(hz)),
    }
}

fn xid_label((t, sub): (u8, u8)) -> String {
    let name = match (t, sub) {
        (0, 0) => return "none: no XID descriptor (some 3rd-party pads)".into(),
        (1, 1) => "gamepad (Duke)",
        (1, 2) => "gamepad (S)",
        (1, 0x10) => "steering wheel",
        (1, 0x20) => "arcade stick",
        (1, _) => "gamepad",
        _ => "unknown",
    };
    format!("{t:02x}/{sub:02x} {name}")
}

/// Analog values as bars, with the number on each bar for screen readers and exact reading.
fn analog_bars(ui: &mut Ui, x: &Xid) {
    for (i, name) in IN_ANALOG.iter().enumerate() {
        let val = x.analog[i];
        ui.horizontal(|ui| {
            ui.add_sized([44.0, 16.0], egui::Label::new(*name));
            ui.add(
                egui::ProgressBar::new(val as f32 / 255.0)
                    .desired_width(150.0)
                    .text(val.to_string()),
            );
        });
    }
}

const GREEN: Color32 = Color32::from_rgb(0x3c, 0xb0, 0x43);
const RED: Color32 = Color32::from_rgb(0xd0, 0x3a, 0x3a);
const BLUE: Color32 = Color32::from_rgb(0x3a, 0x70, 0xd0);
const YELLOW: Color32 = Color32::from_rgb(0xe0, 0xc0, 0x30);
const PAD_WHITE: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
const PAD_BLACK: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);

/// A schematic controller that lights up with the input.
fn pad_drawing(ui: &mut Ui, x: &Xid) {
    let (rect, _) = ui.allocate_exact_size(vec2(380.0, 230.0), Sense::hover());
    let p = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    let at = |px: f32, py: f32| rect.min + vec2(px, py);
    let accent = vis.selection.bg_fill;
    let idle = vis.widgets.inactive.bg_fill;
    let ink = vis.strong_text_color();

    // Body and grips
    let body = vis.extreme_bg_color;
    p.circle_filled(at(95.0, 160.0), 55.0, body);
    p.circle_filled(at(285.0, 160.0), 55.0, body);
    p.rect_filled(
        Rect::from_min_max(at(35.0, 30.0), at(345.0, 160.0)),
        CornerRadius::same(60),
        body,
    );

    // Triggers: bars that fill with pressure
    for (i, x0, label) in [(6, 55.0, "LT"), (7, 235.0, "RT")] {
        let r = Rect::from_min_max(at(x0, 6.0), at(x0 + 90.0, 20.0));
        p.rect_filled(r, CornerRadius::same(3), idle);
        let fill = Rect::from_min_max(
            r.min,
            pos2(r.min.x + r.width() * x.analog[i] as f32 / 255.0, r.max.y),
        );
        p.rect_filled(fill, CornerRadius::same(3), accent);
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.0),
            ink,
        );
    }

    // Sticks: ring, and a knob that moves; the knob lights up when the stick is clicked
    for (c, sx, sy, bit) in [
        (at(95.0, 80.0), x.sticks[0], x.sticks[1], 0x40),
        (at(235.0, 140.0), x.sticks[2], x.sticks[3], 0x80),
    ] {
        p.circle_stroke(c, 24.0, Stroke::new(1.5, vis.weak_text_color()));
        let knob = c + vec2(sx as f32 / 32767.0 * 14.0, -(sy as f32) / 32767.0 * 14.0);
        p.circle_filled(
            knob,
            10.0,
            if x.digital & bit != 0 {
                accent
            } else {
                vis.weak_text_color()
            },
        );
    }

    // D-pad
    let c = at(140.0, 140.0);
    p.rect_filled(
        Rect::from_center_size(c, vec2(14.0, 14.0)),
        CornerRadius::same(2),
        idle,
    );
    for (bit, dx, dy) in [
        (0x01u8, 0.0, -1.0),
        (0x02, 0.0, 1.0),
        (0x04, -1.0, 0.0),
        (0x08, 1.0, 0.0),
    ] {
        let r = Rect::from_center_size(c + vec2(dx * 15.0, dy * 15.0), vec2(14.0, 14.0));
        p.rect_filled(
            r,
            CornerRadius::same(3),
            if x.digital & bit != 0 { accent } else { idle },
        );
    }

    // Back / Start
    for (pos, bit, label) in [
        (at(170.0, 100.0), 0x20u8, "Back"),
        (at(205.0, 100.0), 0x10, "Start"),
    ] {
        p.circle_filled(pos, 6.0, if x.digital & bit != 0 { accent } else { idle });
        p.text(
            pos + vec2(0.0, 13.0),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(9.0),
            ink,
        );
    }

    // Face buttons, Black and White: the coloured disc grows with pressure
    let c = at(290.0, 80.0);
    let buttons = [
        (c + vec2(0.0, 20.0), 11.0, GREEN, x.analog[0], "A"),
        (c + vec2(20.0, 0.0), 11.0, RED, x.analog[1], "B"),
        (c + vec2(-20.0, 0.0), 11.0, BLUE, x.analog[2], "X"),
        (c + vec2(0.0, -20.0), 11.0, YELLOW, x.analog[3], "Y"),
        (at(270.0, 120.0), 8.0, PAD_WHITE, x.analog[5], ""),
        (at(310.0, 120.0), 8.0, PAD_BLACK, x.analog[4], ""),
    ];
    for (pos, r, color, v, label) in buttons {
        p.circle_filled(pos, r, color.gamma_multiply(0.2));
        if v > 0 {
            p.circle_filled(pos, r * (0.35 + 0.65 * v as f32 / 255.0), color);
        }
        p.circle_stroke(pos, r, Stroke::new(1.0, color));
        if !label.is_empty() {
            p.text(
                pos,
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                ink,
            );
        }
    }
    p.text(
        at(270.0, 133.0),
        Align2::CENTER_CENTER,
        "White",
        FontId::proportional(9.0),
        ink,
    );
    p.text(
        at(310.0, 133.0),
        Align2::CENTER_CENTER,
        "Black",
        FontId::proportional(9.0),
        ink,
    );
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
