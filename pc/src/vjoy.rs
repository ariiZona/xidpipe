// SPDX-License-Identifier: GPL-3.0-or-later
//! vJoy backend (Windows, DirectInput).
//!
//! Pad N gets one vJoy device, `vjoy_device + (N-1)`, so emulators see one device per pad.
//! Axes: X/Y and RX/RY = sticks, Z/RZ = lt/rt, Slider/Dial = sl0/sl1, 11 buttons, d-pad on POV 1.
//! DirectInput has 8 axes per device, so up to 4 pressure buttons get their own axis: map them
//! to lt/rt/sl0/sl1.

use crate::output::Output;
use crate::pad::{Out, Profile};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;

/// D-pad bits -> 8-way hat (0 = N, clockwise to 7 = NW), None when released.
pub fn hat8(buttons: u16) -> Option<u8> {
    let on = |b: u16| (buttons & b != 0) as i8;
    let (x, y) = (on(0x0008) - on(0x0004), on(0x0001) - on(0x0002));
    Some(match (x, y) {
        (0, 1) => 0,
        (1, 1) => 1,
        (1, 0) => 2,
        (1, -1) => 3,
        (0, -1) => 4,
        (-1, -1) => 5,
        (-1, 0) => 6,
        (-1, 1) => 7,
        _ => return None,
    })
}

/// JOYSTICK_POSITION_V2 from vJoy's SDK public.h.
#[repr(C)]
#[derive(Default, Debug, PartialEq)]
struct Pos {
    device: u8,
    throttle: i32,
    rudder: i32,
    aileron: i32,
    x: i32,
    y: i32,
    z: i32,
    rx: i32,
    ry: i32,
    rz: i32,
    slider: i32,
    dial: i32,
    wheel: i32,
    vx: i32,
    vy: i32,
    vz: i32,
    vbrx: i32,
    vbry: i32,
    vbrz: i32,
    buttons: u32,
    hats: [u32; 4],
    buttons_ex: [u32; 3],
}
const _: () = assert!(std::mem::size_of::<Pos>() == 108);

/// XInput button bits in DirectInput button order (button 1 = A ... 11 = guide).
const BUTTONS: [u16; 11] = [
    0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0x0020, 0x0010, 0x0040, 0x0080, 0x0400,
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pov {
    Continuous,
    Discrete,
    Missing,
}

/// vJoy axes run 0..=32767; DirectInput +Y is down.
fn axis(v: i16) -> i32 {
    (v as i32 + 32768) >> 1
}
fn axis8(v: u8) -> i32 {
    v as i32 * 32767 / 255
}

fn pad_pos(o: &Out, pov: Pov) -> Pos {
    let [lx, ly, rx, ry] = o.sticks;
    let hat = hat8(o.buttons);
    let pov_value = match (pov, hat) {
        (_, None) | (Pov::Missing, _) => u32::MAX,     // neutral
        (Pov::Continuous, Some(h)) => h as u32 * 4500, // hundredths of a degree
        // Discrete POVs only know N/E/S/W: diagonals keep their vertical part.
        (Pov::Discrete, Some(h)) => 0xFFFF_FFF0 | [0, 0, 1, 2, 2, 2, 3, 0][h as usize],
    };
    Pos {
        x: axis(lx),
        y: axis(ly.saturating_neg()),
        z: axis8(o.lt),
        rx: axis(rx),
        ry: axis(ry.saturating_neg()),
        rz: axis8(o.rt),
        slider: axis8(o.sliders[0]),
        dial: axis8(o.sliders[1]),
        buttons: BUTTONS
            .iter()
            .enumerate()
            .filter(|b| o.buttons & b.1 != 0)
            .fold(0, |acc, b| acc | 1 << b.0),
        hats: [pov_value, u32::MAX, u32::MAX, u32::MAX],
        ..Pos::default()
    }
}

struct Api {
    enabled: unsafe extern "C" fn() -> i32,
    status: unsafe extern "C" fn(u32) -> i32,
    acquire: unsafe extern "C" fn(u32) -> i32,
    relinquish: unsafe extern "C" fn(u32),
    update: unsafe extern "C" fn(u32, *mut c_void) -> i32,
    buttons: unsafe extern "C" fn(u32) -> i32,
    cont_povs: unsafe extern "C" fn(u32) -> i32,
    disc_povs: unsafe extern "C" fn(u32) -> i32,
    axis_exists: unsafe extern "C" fn(u32, u32) -> i32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let arch = if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    };
    let mut paths = vec![PathBuf::from("vJoyInterface.dll")];
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        paths.push(
            PathBuf::from(pf)
                .join("vJoy")
                .join(arch)
                .join("vJoyInterface.dll"),
        );
    }
    let lib = paths
        .iter()
        .find_map(|p| {
            let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
            let h = unsafe { LoadLibraryW(wide.as_ptr()) };
            (!h.is_null()).then_some(h)
        })
        .ok_or("vJoyInterface.dll not found; is vJoy installed? https://github.com/BrunnerInnovation/vJoy/releases")?;
    let f = |name: &str| -> Result<*mut c_void, String> {
        let c = format!("{name}\0");
        let p = unsafe { GetProcAddress(lib, c.as_ptr()) };
        if p.is_null() {
            Err(format!("vJoyInterface.dll has no {name}"))
        } else {
            Ok(p)
        }
    };
    // Safety: signatures from vJoy's SDK vjoyinterface.h (all __cdecl). Each transmute's
    // target type is the Api field it is assigned to.
    #[allow(clippy::missing_transmute_annotations)]
    unsafe {
        use std::mem::transmute as t;
        Ok(Api {
            enabled: t(f("vJoyEnabled")?),
            status: t(f("GetVJDStatus")?),
            acquire: t(f("AcquireVJD")?),
            relinquish: t(f("RelinquishVJD")?),
            update: t(f("UpdateVJD")?),
            buttons: t(f("GetVJDButtonNumber")?),
            cont_povs: t(f("GetVJDContPovNumber")?),
            disc_povs: t(f("GetVJDDiscPovNumber")?),
            axis_exists: t(f("GetVJDAxisExist")?),
        })
    }
}

/// An acquired vJoy device; released (and centred) on drop.
struct Dev {
    api: &'static Api,
    id: u32,
    neutral: Pos,
}

impl Dev {
    fn acquire(api: &'static Api, id: u32) -> Result<Dev, String> {
        match unsafe { (api.status)(id) } {
            0 | 1 => {} // owned by us / free
            2 => return Err(format!("vJoy device {id} is in use by another program")),
            3 => {
                return Err(format!(
                    "vJoy device {id} doesn't exist; add it in \"Configure vJoy\""
                ));
            }
            _ => return Err(format!("vJoy device {id}: unknown status")),
        }
        if unsafe { (api.acquire)(id) } == 0 {
            return Err(format!("couldn't acquire vJoy device {id}"));
        }
        Ok(Dev {
            api,
            id,
            neutral: Pos::default(),
        })
    }

    fn update(&self, mut pos: Pos) -> Result<(), String> {
        pos.device = self.id as u8;
        if unsafe { (self.api.update)(self.id, &mut pos as *mut Pos as *mut c_void) } == 0 {
            return Err(format!("vJoy device {} update failed", self.id));
        }
        Ok(())
    }

    fn missing_axes(&self) -> Vec<&'static str> {
        [
            (0x30, "X"),
            (0x31, "Y"),
            (0x32, "Z"),
            (0x33, "RX"),
            (0x34, "RY"),
            (0x35, "RZ"),
            (0x36, "Slider"),
            (0x37, "Dial"),
        ]
        .into_iter()
        .filter(|a| unsafe { (self.api.axis_exists)(self.id, a.0) } == 0)
        .map(|a| a.1)
        .collect()
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        let neutral = std::mem::take(&mut self.neutral);
        let _ = self.update(neutral);
        unsafe { (self.api.relinquish)(self.id) };
    }
}

struct VJoy {
    dev: Dev,
    pov: Pov,
}

pub fn open(p: &Profile, pad: u8) -> Result<Box<dyn Output>, String> {
    let api = api()?;
    if unsafe { (api.enabled)() } == 0 {
        return Err("vJoy is installed but disabled; enable it in \"Configure vJoy\"".into());
    }
    let id = p.vjoy_device as u32 + pad as u32 - 1;
    if id > 16 {
        return Err(format!(
            "pad {pad} needs vJoy device {id}, but vJoy has 16 at most"
        ));
    }
    let mut dev = Dev::acquire(api, id)?;

    // Report setup problems once instead of failing: the pad still works with what exists.
    let warn = |m: String| eprintln!("xidpipe: vJoy device {id}: {m}");
    let missing = dev.missing_axes();
    if !missing.is_empty() {
        warn(format!(
            "missing axes {missing:?}; outputs mapped there won't reach games"
        ));
    }
    let buttons = unsafe { (api.buttons)(id) };
    if buttons < BUTTONS.len() as i32 {
        warn(format!(
            "has {buttons} buttons, xidpipe uses {}",
            BUTTONS.len()
        ));
    }
    let pov = if unsafe { (api.cont_povs)(id) } > 0 {
        Pov::Continuous
    } else if unsafe { (api.disc_povs)(id) } > 0 {
        Pov::Discrete
    } else {
        warn("has no POV hat, so the d-pad won't work".into());
        Pov::Missing
    };
    dev.neutral = pad_pos(&Out::default(), pov);
    Ok(Box::new(VJoy { dev, pov }))
}

// shortcut: no rumble for vJoy; its force feedback is DirectInput effects (FfbRegisterGenCB),
// which would need an effect-to-motor translation. Add it if a game needs vJoy + rumble.
impl Output for VJoy {
    fn send(&mut self, o: &Out) -> Result<(), String> {
        self.dev.update(pad_pos(o, self.pov))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hats() {
        assert_eq!(hat8(0), None);
        assert_eq!(hat8(0x0001), Some(0));
        assert_eq!(hat8(0x0001 | 0x0008), Some(1));
        assert_eq!(hat8(0x0002 | 0x0004), Some(5));
        assert_eq!(hat8(0x0001 | 0x0002), None); // opposite directions cancel
    }

    #[test]
    fn positions() {
        let o = Out {
            buttons: 0x1000 | 0x0400 | 0x0001 | 0x0008,
            lt: 255,
            sliders: [255, 128],
            sticks: [32767, 32767, -32768, 0],
            ..Out::default()
        };
        let p = pad_pos(&o, Pov::Continuous);
        assert_eq!(
            (p.x, p.y, p.z, p.rx, p.ry, p.rz),
            (32767, 0, 32767, 0, 16384, 0)
        );
        assert_eq!(p.buttons, 1 | 1 << 10);
        assert_eq!(p.hats[0], 4500);
        assert_eq!(pad_pos(&o, Pov::Discrete).hats[0], 0xFFFF_FFF0); // NE -> N
        assert_eq!(pad_pos(&Out::default(), Pov::Discrete).hats[0], u32::MAX);
        assert_eq!((p.slider, p.dial), (32767, 16447));
    }
}
