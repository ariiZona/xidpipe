// SPDX-License-Identifier: GPL-3.0-or-later
//! XID report decoding, presets, deadzones and button mapping.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One OG Xbox gamepad report, decoded. +Y is up on both sticks.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Xid {
    /// bit0 up, 1 down, 2 left, 3 right, 4 start, 5 back, 6 ls, 7 rs (same bits as XInput's low byte)
    pub digital: u8,
    /// A, B, X, Y, Black, White, LT, RT pressure (0-255)
    pub analog: [u8; 8],
    /// lx, ly, rx, ry
    pub sticks: [i16; 4],
}

/// Decode a raw XID input report. Some 3rd-party pads send short reports,
/// so missing bytes read as zero instead of rejecting the report.
pub fn parse(r: &[u8]) -> Xid {
    let mut b = [0u8; 20];
    let n = r.len().min(20);
    b[..n].copy_from_slice(&r[..n]);
    Xid {
        digital: b[2],
        analog: b[4..12].try_into().unwrap(),
        sticks: [0, 1, 2, 3].map(|i| i16::from_le_bytes([b[12 + 2 * i], b[13 + 2 * i]])),
    }
}

/// Virtual pad state in XInput terms; every backend converts from this.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Out {
    /// XUSB_GAMEPAD button bits
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    /// lx, ly, rx, ry (+Y up)
    pub sticks: [i16; 4],
    /// A B X Y Black White pressure after the analog deadzone; all 0 unless `pressure` is on.
    /// Only backends with spare axes (vJoy) use it.
    pub pressure: [u8; 6],
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Xinput,
    Dinput,
    Vjoy,
    None,
}

impl Backend {
    pub const ALL: [Backend; 4] = [
        Backend::Xinput,
        Backend::Dinput,
        Backend::Vjoy,
        Backend::None,
    ];
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Preset {
    pub backend: Backend,
    pub pressure: bool,
    pub press_threshold: u8,
    pub analog_deadzone: u8,
    pub left_stick_deadzone: u16,
    pub right_stick_deadzone: u16,
    /// First vJoy device to use (backend = "vjoy")
    pub vjoy_device: u8,
    pub map: BTreeMap<String, String>,
}

impl Default for Preset {
    /// Same values as presets/xbox.toml (a test checks they match).
    fn default() -> Self {
        Preset {
            backend: Backend::Xinput,
            pressure: false,
            press_threshold: 30,
            analog_deadzone: 30,
            left_stick_deadzone: 7849,
            right_stick_deadzone: 8689,
            vjoy_device: 1,
            map: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Src {
    None,
    Analog(usize),
    Digital(u8),
    Axis(usize),
}

#[derive(Clone, Copy, Debug)]
enum Target {
    Button(u16),
    Trigger(usize),
    Axis(usize),
}

fn src(name: &str) -> Option<Src> {
    use Src::*;
    Some(match name {
        "none" => None,
        "a" => Analog(0),
        "b" => Analog(1),
        "x" => Analog(2),
        "y" => Analog(3),
        "black" => Analog(4),
        "white" => Analog(5),
        "lt" => Analog(6),
        "rt" => Analog(7),
        "up" => Digital(0x01),
        "down" => Digital(0x02),
        "left" => Digital(0x04),
        "right" => Digital(0x08),
        "start" => Digital(0x10),
        "back" => Digital(0x20),
        "ls" => Digital(0x40),
        "rs" => Digital(0x80),
        "lx" => Axis(0),
        "ly" => Axis(1),
        "rx" => Axis(2),
        "ry" => Axis(3),
        _ => return Option::None,
    })
}

/// (output name, output kind, default source)
const TARGETS: &[(&str, Target, &str)] = &[
    ("a", Target::Button(0x1000), "a"),
    ("b", Target::Button(0x2000), "b"),
    ("x", Target::Button(0x4000), "x"),
    ("y", Target::Button(0x8000), "y"),
    ("lb", Target::Button(0x0100), "white"),
    ("rb", Target::Button(0x0200), "black"),
    ("back", Target::Button(0x0020), "back"),
    ("start", Target::Button(0x0010), "start"),
    ("ls", Target::Button(0x0040), "ls"),
    ("rs", Target::Button(0x0080), "rs"),
    ("up", Target::Button(0x0001), "up"),
    ("down", Target::Button(0x0002), "down"),
    ("left", Target::Button(0x0004), "left"),
    ("right", Target::Button(0x0008), "right"),
    ("guide", Target::Button(0x0400), "none"),
    ("lt", Target::Trigger(0), "lt"),
    ("rt", Target::Trigger(1), "rt"),
    ("lx", Target::Axis(0), "lx"),
    ("ly", Target::Axis(1), "ly"),
    ("rx", Target::Axis(2), "rx"),
    ("ry", Target::Axis(3), "ry"),
];

/// Inputs a button or trigger output can take.
pub const BUTTON_INPUTS: [&str; 17] = [
    "none", "a", "b", "x", "y", "black", "white", "lt", "rt", "up", "down", "left", "right",
    "start", "back", "ls", "rs",
];
/// Inputs a stick axis output can take.
pub const AXIS_INPUTS: [&str; 5] = ["none", "lx", "ly", "rx", "ry"];

/// Every output as (name, is a stick axis, default input), in display order.
pub fn outputs() -> impl Iterator<Item = (&'static str, bool, &'static str)> {
    TARGETS
        .iter()
        .map(|t| (t.0, matches!(t.1, Target::Axis(_)), t.2))
}

/// A validated preset, ready to apply to reports.
#[derive(Clone)]
pub struct Profile {
    pub backend: Backend,
    pub vjoy_device: u8,
    pub pressure: bool,
    press: u8,
    analog_dz: u8,
    stick_dz: [u16; 2],
    map: Vec<(Target, Src)>,
}

impl Profile {
    pub fn new(p: &Preset) -> Result<Profile, String> {
        for (k, dz) in [
            ("left", p.left_stick_deadzone),
            ("right", p.right_stick_deadzone),
        ] {
            if dz > 32000 {
                return Err(format!("{k}_stick_deadzone must be 0..=32000, got {dz}"));
            }
        }
        if !(1..=16).contains(&p.vjoy_device) {
            return Err(format!("vjoy_device must be 1..=16, got {}", p.vjoy_device));
        }
        if let Some(bad) = p.map.keys().find(|k| !TARGETS.iter().any(|t| t.0 == *k)) {
            return Err(format!("map: unknown output '{bad}'"));
        }
        let mut map = Vec::new();
        for &(name, target, default) in TARGETS {
            let s = p.map.get(name).map_or(default, String::as_str);
            let source = src(s).ok_or(format!("map: unknown input '{s}' for '{name}'"))?;
            let fits = match (target, source) {
                (_, Src::None) => true,
                (Target::Axis(_), Src::Axis(_)) => true,
                (Target::Axis(_), _) | (_, Src::Axis(_)) => false,
                _ => true,
            };
            if !fits {
                return Err(format!(
                    "map: '{name} = \"{s}\"' mixes a stick axis with a button"
                ));
            }
            map.push((target, source));
        }
        Ok(Profile {
            backend: p.backend,
            vjoy_device: p.vjoy_device,
            pressure: p.pressure,
            press: p.press_threshold,
            analog_dz: p.analog_deadzone,
            stick_dz: [p.left_stick_deadzone, p.right_stick_deadzone],
            map,
        })
    }

    pub fn apply(&self, x: &Xid) -> Out {
        let (lx, ly) = stick(x.sticks[0], x.sticks[1], self.stick_dz[0]);
        let (rx, ry) = stick(x.sticks[2], x.sticks[3], self.stick_dz[1]);
        let sticks = [lx, ly, rx, ry];
        let mut o = Out::default();
        if self.pressure {
            o.pressure = std::array::from_fn(|i| dz8(x.analog[i], self.analog_dz));
        }
        for &(t, s) in &self.map {
            match t {
                Target::Button(bit) if self.pressed(x, s) => o.buttons |= bit,
                Target::Button(_) => {}
                Target::Trigger(i) => {
                    let v = self.level(x, s);
                    if i == 0 { o.lt = v } else { o.rt = v }
                }
                Target::Axis(i) => o.sticks[i] = if let Src::Axis(j) = s { sticks[j] } else { 0 },
            }
        }
        o
    }

    /// Digital view of a source: analog buttons count as pressed above press_threshold.
    fn pressed(&self, x: &Xid, s: Src) -> bool {
        match s {
            Src::Analog(i) => x.analog[i] > self.press,
            Src::Digital(bit) => x.digital & bit != 0,
            _ => false,
        }
    }

    /// Analog view of a source (0-255). Face buttons only give pressure when `pressure` is on.
    fn level(&self, x: &Xid, s: Src) -> u8 {
        match s {
            Src::Analog(i) if i >= 6 || self.pressure => dz8(x.analog[i], self.analog_dz),
            _ if self.pressed(x, s) => 255,
            _ => 0,
        }
    }
}

/// Deadzone on a 0-255 value, rescaled so the output still reaches 255.
fn dz8(v: u8, t: u8) -> u8 {
    if v <= t {
        0
    } else {
        ((v - t) as u32 * 255 / (255 - t) as u32) as u8
    }
}

/// Scaled radial deadzone: no output inside `dz`, full range outside it.
fn stick(x: i16, y: i16, dz: u16) -> (i16, i16) {
    if dz == 0 {
        return (x, y);
    }
    let (fx, fy) = (x as f32, y as f32);
    let m = (fx * fx + fy * fy).sqrt();
    if m <= dz as f32 {
        return (0, 0);
    }
    let s = ((m - dz as f32) / (32767.0 - dz as f32)).min(1.0) * 32767.0 / m;
    ((fx * s) as i16, (fy * s) as i16) // float->int `as` saturates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(digital: u8, analog: [u8; 8], sticks: [i16; 4]) -> Vec<u8> {
        let mut r = vec![0, 20, digital, 0];
        r.extend(analog);
        sticks.iter().for_each(|s| r.extend(s.to_le_bytes()));
        r
    }

    fn preset(src: &str) -> Profile {
        Profile::new(&toml::from_str(src).unwrap()).unwrap()
    }

    #[test]
    fn parses_full_and_short_reports() {
        let x = parse(&report(
            0x11,
            [1, 2, 3, 4, 5, 6, 7, 8],
            [-32768, 32767, 5, -5],
        ));
        assert_eq!(x.digital, 0x11);
        assert_eq!(x.analog, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(x.sticks, [-32768, 32767, 5, -5]);
        let short = parse(&[0, 6, 0x01, 0, 200, 0]);
        assert_eq!(short.analog[0], 200);
        assert_eq!(short.sticks, [0; 4]);
        assert_eq!(parse(&[]), Xid::default());
    }

    #[test]
    fn builtin_presets_load() {
        for p in crate::BUILTIN {
            Profile::new(&toml::from_str(p.1).unwrap()).unwrap();
        }
        let x: Preset = toml::from_str(crate::BUILTIN[0].1).unwrap();
        let d = Preset::default();
        assert_eq!(
            (
                x.backend,
                x.pressure,
                x.press_threshold,
                x.analog_deadzone,
                x.left_stick_deadzone,
                x.right_stick_deadzone
            ),
            (
                d.backend,
                d.pressure,
                d.press_threshold,
                d.analog_deadzone,
                d.left_stick_deadzone,
                d.right_stick_deadzone
            )
        );
    }

    #[test]
    fn default_map_and_xbox_deadzones() {
        let p = Profile::new(&Preset::default()).unwrap();
        let o = p.apply(&parse(&report(
            0x01 | 0x10,
            [255, 0, 0, 0, 255, 255, 20, 255],
            [7000, 0, 32767, 0],
        )));
        assert_eq!(o.buttons, 0x1000 | 0x0001 | 0x0010 | 0x0100 | 0x0200);
        assert_eq!((o.lt, o.rt), (0, 255)); // 20 is inside the 30 analog deadzone
        assert_eq!(o.sticks[0], 0); // 7000 is inside the 7849 left deadzone
        assert_eq!(o.sticks[2], 32767);
    }

    #[test]
    fn zero_deadzone_passes_through() {
        let p = preset(crate::BUILTIN[1].1);
        let o = p.apply(&parse(&report(
            0,
            [0, 0, 0, 0, 0, 0, 1, 0],
            [1, -1, -32768, 32767],
        )));
        assert_eq!(o.sticks, [1, -1, -32768, 32767]);
        assert_eq!(o.lt, 1);
    }

    #[test]
    fn pressure_toggle() {
        let x = parse(&report(0, [128, 0, 0, 0, 0, 0, 0, 0], [0; 4]));
        let on = preset("pressure = true\nanalog_deadzone = 0\nmap = { rt = \"a\" }");
        let off = preset("pressure = false\nanalog_deadzone = 0\nmap = { rt = \"a\" }");
        assert_eq!(on.apply(&x).rt, 128);
        assert_eq!(off.apply(&x).rt, 255);
        assert_eq!(on.apply(&x).buttons & 0x1000, 0x1000); // still a digital A too
        assert_eq!(on.apply(&x).pressure, [128, 0, 0, 0, 0, 0]);
        assert_eq!(off.apply(&x).pressure, [0; 6]);
    }

    #[test]
    fn preset_round_trips_through_toml() {
        let mut p = Preset {
            backend: Backend::Vjoy,
            ..Preset::default()
        };
        p.map.insert("rt".into(), "a".into());
        let back: Preset = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        assert!(Profile::new(&toml::from_str("vjoy_device = 0").unwrap()).is_err());
    }

    #[test]
    fn input_lists_match_parser() {
        for i in BUTTON_INPUTS.iter().chain(&AXIS_INPUTS) {
            assert!(src(i).is_some(), "{i}");
        }
        for (name, axis, default) in outputs() {
            let list: &[&str] = if axis { &AXIS_INPUTS } else { &BUTTON_INPUTS };
            assert!(list.contains(&default), "{name}");
        }
    }

    #[test]
    fn deadzone_math() {
        assert_eq!(dz8(255, 30), 255);
        assert_eq!(dz8(30, 30), 0);
        assert_eq!(dz8(200, 255), 0);
        assert_eq!(stick(-32768, -32768, 8000), (-23169, -23169));
        let (x, _) = stick(32767, 0, 8000);
        assert_eq!(x, 32767);
    }

    #[test]
    fn rejects_bad_maps() {
        let bad = |s: &str| Profile::new(&toml::from_str(s).unwrap()).is_err();
        assert!(bad("map = { lx = \"a\" }"));
        assert!(bad("map = { a = \"lx\" }"));
        assert!(bad("map = { nope = \"a\" }"));
        assert!(bad("map = { a = \"nope\" }"));
        assert!(bad("left_stick_deadzone = 40000"));
        assert!(toml::from_str::<Preset>("typo = 1").is_err());
    }
}
