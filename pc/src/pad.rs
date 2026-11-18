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
    /// Extra analog outputs sl0, sl1 (0-255). Only vJoy has axes for them (Slider, Dial).
    pub sliders: [u8; 2],
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
    /// Size of a per-axis dead zone the controller applies itself (0 = none). Values just
    /// outside it are rescaled to start at 0, so the stick no longer jumps at its edge.
    pub left_snap_zone: u16,
    pub right_snap_zone: u16,
    /// First vJoy device to use (backend = "vjoy")
    pub vjoy_device: u8,
    /// Vibration strength in percent: scales both motors, so it is also the maximum (0 = off)
    pub rumble_strength: u8,
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
            left_snap_zone: 0,
            right_snap_zone: 0,
            vjoy_device: 1,
            rumble_strength: 100,
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
    ("sl0", Target::Trigger(2), "none"),
    ("sl1", Target::Trigger(3), "none"),
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
    pub rumble_strength: u8,
    pressure: bool,
    press: u8,
    analog_dz: u8,
    stick_dz: [u16; 2],
    snap_zone: [u16; 2],
    map: Vec<(Target, Src)>,
}

impl Profile {
    pub fn new(p: &Preset) -> Result<Profile, String> {
        for (k, dz) in [
            ("left_stick_deadzone", p.left_stick_deadzone),
            ("right_stick_deadzone", p.right_stick_deadzone),
            ("left_snap_zone", p.left_snap_zone),
            ("right_snap_zone", p.right_snap_zone),
        ] {
            if dz > 32000 {
                return Err(format!("{k} must be 0..=32000, got {dz}"));
            }
        }
        if p.rumble_strength > 100 {
            return Err(format!(
                "rumble_strength must be 0..=100, got {}",
                p.rumble_strength
            ));
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
            rumble_strength: p.rumble_strength,
            pressure: p.pressure,
            press: p.press_threshold,
            analog_dz: p.analog_deadzone,
            stick_dz: [p.left_stick_deadzone, p.right_stick_deadzone],
            snap_zone: [p.left_snap_zone, p.right_snap_zone],
            map,
        })
    }

    pub fn apply(&self, x: &Xid) -> Out {
        let [s0, s1, s2, s3] = x.sticks;
        let [zl, zr] = self.snap_zone;
        let (lx, ly) = stick(unsnap(s0, zl), unsnap(s1, zl), self.stick_dz[0]);
        let (rx, ry) = stick(unsnap(s2, zr), unsnap(s3, zr), self.stick_dz[1]);
        let sticks = [lx, ly, rx, ry];
        let mut o = Out::default();
        for &(t, s) in &self.map {
            match t {
                Target::Button(bit) if self.pressed(x, s) => o.buttons |= bit,
                Target::Button(_) => {}
                Target::Trigger(i) => {
                    let v = self.level(x, s);
                    match i {
                        0 => o.lt = v,
                        1 => o.rt = v,
                        _ => o.sliders[i - 2] = v,
                    }
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

/// Undo the jump of a controller's own per-axis dead zone of size `zone`: the controller sends
/// 0 inside it and the real value outside, so values outside are rescaled to start from 0.
/// What happens inside the zone can't be recovered; the controller never sends it.
fn unsnap(v: i16, zone: u16) -> i16 {
    let (a, z) = ((v as i32).abs(), zone as i32);
    if z == 0 || a == 0 {
        return v;
    }
    if a <= z {
        return 0;
    }
    ((a - z) * 32767 / (32767 - z) * (v as i32).signum()).clamp(-32768, 32767) as i16
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

/// Recent raw stick positions and the smallest non-zero value seen per axis, for the
/// Button tester's trace and its snap-zone measurement.
#[cfg_attr(test, allow(dead_code))] // the window is left out of test builds
pub struct Trace {
    pub points: [std::collections::VecDeque<(i16, i16)>; 2],
    /// [stick][axis]: smallest non-zero |value| seen (u16::MAX = none yet)
    pub min_nonzero: [[u16; 2]; 2],
}

const TRACE_POINTS: usize = 600;

impl Default for Trace {
    fn default() -> Self {
        Trace {
            points: Default::default(),
            min_nonzero: [[u16::MAX; 2]; 2],
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
impl Trace {
    pub fn add(&mut self, x: &Xid) {
        for (stick, pts) in self.points.iter_mut().enumerate() {
            let p = (x.sticks[stick * 2], x.sticks[stick * 2 + 1]);
            if pts.back() != Some(&p) {
                if pts.len() == TRACE_POINTS {
                    pts.pop_front();
                }
                pts.push_back(p);
            }
            for (axis, v) in [p.0, p.1].into_iter().enumerate() {
                let a = v.unsigned_abs();
                if a != 0 {
                    let m = &mut self.min_nonzero[stick][axis];
                    *m = (*m).min(a);
                }
            }
        }
    }

    /// The controller's own per-axis dead zone, if the trace shows one: no axis ever reported
    /// a small non-zero value. Small minimums are just sensor resolution.
    pub fn snap_zone(&self, stick: usize) -> Option<u16> {
        let m = self.min_nonzero[stick][0].min(self.min_nonzero[stick][1]);
        (m != u16::MAX && m > 1024).then_some(m - 1)
    }
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
        // Every setting in xbox.toml equals the default. Its map only spells out default mappings.
        let x: Preset = toml::from_str(crate::BUILTIN[0].1).unwrap();
        let d = Preset::default();
        assert_eq!(
            Preset {
                map: d.map.clone(),
                ..x
            },
            d
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
        let on_slider = preset(
            "pressure = true
analog_deadzone = 0
map = { sl1 = \"a\" }",
        );
        assert_eq!(on_slider.apply(&x).sliders, [0, 128]);
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
        assert!(Profile::new(&toml::from_str("rumble_strength = 101").unwrap()).is_err());
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
    fn trace_finds_snap_zone() {
        let mut t = Trace::default();
        assert_eq!(t.snap_zone(0), None);
        for s in [
            [0, 32000, 0, 0],
            [2560, 31000, 300, 0],
            [0, -32000, -256, 0],
            [-2816, 0, 0, 0],
        ] {
            t.add(&Xid {
                sticks: s,
                ..Xid::default()
            });
        }
        assert_eq!(t.min_nonzero[0], [2560, 31000]);
        assert_eq!(t.snap_zone(0), Some(2559)); // left never reported anything below 2560
        assert_eq!(t.snap_zone(1), None); // right reported 256: just sensor resolution
        assert_eq!(t.points[0].len(), 4);
    }

    #[test]
    fn snap_zone_correction() {
        assert_eq!(unsnap(5000, 0), 5000);
        assert_eq!(unsnap(0, 3000), 0);
        assert_eq!(unsnap(3000, 3000), 0);
        assert_eq!(unsnap(3001, 3000), 1); // just past the zone: starts at 0, no jump
        assert_eq!(unsnap(-3100, 3000), -110);
        assert_eq!(unsnap(32767, 3000), 32767);
        assert_eq!(unsnap(-32768, 3000), -32768);
        let p = preset("left_stick_deadzone = 0\nleft_snap_zone = 4000");
        let o = p.apply(&parse(&report(0, [0; 8], [4100, 32767, 0, 0])));
        assert_eq!(&o.sticks[..2], &[113, 32767]);
        assert!(Profile::new(&toml::from_str("right_snap_zone = 40000").unwrap()).is_err());
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
