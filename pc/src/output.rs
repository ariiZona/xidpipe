// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual controller backends.

use crate::pad::{Backend, Out};

pub trait Output {
    fn send(&mut self, o: &Out) -> Result<(), String>;
}

struct Null;
impl Output for Null {
    fn send(&mut self, _: &Out) -> Result<(), String> {
        Ok(())
    }
}

pub fn open(b: Backend) -> Result<Box<dyn Output>, String> {
    match b {
        Backend::None => Ok(Box::new(Null)),
        _ => imp::open(b),
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use vigem_client::{
        Client, DS4Report, DualShock4Wired, TargetId, XButtons, XGamepad, Xbox360Wired,
    };

    const VIGEM_HELP: &str = "is ViGEmBus installed? https://github.com/nefarius/ViGEmBus/releases";

    pub fn open(b: Backend) -> Result<Box<dyn Output>, String> {
        let client = Client::connect().map_err(|e| format!("ViGEmBus: {e:?} ({VIGEM_HELP})"))?;
        if b == Backend::Dinput {
            let mut t = DualShock4Wired::new(client, TargetId::DUALSHOCK4_WIRED);
            t.plugin()
                .and_then(|_| t.wait_ready())
                .map_err(|e| format!("ViGEm DS4: {e:?}"))?;
            Ok(Box::new(Ds4(t)))
        } else {
            let mut t = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
            t.plugin()
                .and_then(|_| t.wait_ready())
                .map_err(|e| format!("ViGEm X360: {e:?}"))?;
            Ok(Box::new(X360(t)))
        }
    }

    struct X360(Xbox360Wired<Client>);
    impl Output for X360 {
        fn send(&mut self, o: &Out) -> Result<(), String> {
            let [lx, ly, rx, ry] = o.sticks;
            let g = XGamepad {
                buttons: XButtons(o.buttons),
                left_trigger: o.lt,
                right_trigger: o.rt,
                thumb_lx: lx,
                thumb_ly: ly,
                thumb_rx: rx,
                thumb_ry: ry,
            };
            self.0.update(&g).map_err(|e| format!("{e:?}"))
        }
    }

    struct Ds4(DualShock4Wired<Client>);
    impl Output for Ds4 {
        fn send(&mut self, o: &Out) -> Result<(), String> {
            self.0.update(&ds4(o)).map_err(|e| format!("{e:?}"))
        }
    }

    /// XInput state -> DS4 report (ViGEm DS4_BUTTONS layout, sticks 0-255 with +Y down).
    pub(crate) fn ds4(o: &Out) -> DS4Report {
        let u8x = |v: i16| ((v as i32 + 32768) >> 8) as u8;
        let u8y = |v: i16| u8x(v.saturating_neg());
        let on = |bit: u16| o.buttons & bit != 0;
        const MAP: [(u16, u16); 10] = [
            (0x4000, 1 << 4),  // X -> square
            (0x1000, 1 << 5),  // A -> cross
            (0x2000, 1 << 6),  // B -> circle
            (0x8000, 1 << 7),  // Y -> triangle
            (0x0100, 1 << 8),  // LB -> L1
            (0x0200, 1 << 9),  // RB -> R1
            (0x0020, 1 << 12), // back -> share
            (0x0010, 1 << 13), // start -> options
            (0x0040, 1 << 14), // LS -> L3
            (0x0080, 1 << 15), // RS -> R3
        ];
        let mut buttons = MAP.iter().filter(|m| on(m.0)).fold(0, |acc, m| acc | m.1);
        if o.lt > 0 {
            buttons |= 1 << 10
        }
        if o.rt > 0 {
            buttons |= 1 << 11
        }
        // Hat: 0 = N, clockwise to 7 = NW, 8 = released.
        buttons |= match (on(0x0001), on(0x0002), on(0x0004), on(0x0008)) {
            (true, _, false, true) => 1,
            (true, _, true, _) => 7,
            (true, _, _, _) => 0,
            (_, true, false, true) => 3,
            (_, true, true, _) => 5,
            (_, true, _, _) => 4,
            (_, _, false, true) => 2,
            (_, _, true, _) => 6,
            _ => 8,
        };
        DS4Report {
            thumb_lx: u8x(o.sticks[0]),
            thumb_ly: u8y(o.sticks[1]),
            thumb_rx: u8x(o.sticks[2]),
            thumb_ry: u8y(o.sticks[3]),
            buttons,
            special: on(0x0400) as u8,
            trigger_l: o.lt,
            trigger_r: o.rt,
        }
    }

    #[test]
    fn ds4_conversion() {
        let r = ds4(&Out {
            buttons: 0x1000 | 0x0001 | 0x0008,
            lt: 9,
            rt: 0,
            sticks: [0, 32767, -32768, 0],
        });
        assert_eq!(r.buttons, 1 << 5 | 1 << 10 | 1);
        assert_eq!(
            (r.thumb_lx, r.thumb_ly, r.thumb_rx, r.thumb_ry),
            (128, 0, 0, 128)
        );
        assert_eq!(ds4(&Out::default()).buttons, 8);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use evdev::uinput::VirtualDevice;
    use evdev::{
        AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventType, InputEvent, InputId,
        KeyCode, UinputAbsSetup,
    };

    // Same codes as the kernel's xpad driver, so SDL/Steam/Wine treat "xinput" as a real 360 pad.
    // (xpad reports X as BTN_NORTH and Y as BTN_WEST.)
    const KEYS: [(u16, KeyCode); 11] = [
        (0x1000, KeyCode::BTN_SOUTH),
        (0x2000, KeyCode::BTN_EAST),
        (0x4000, KeyCode::BTN_NORTH),
        (0x8000, KeyCode::BTN_WEST),
        (0x0100, KeyCode::BTN_TL),
        (0x0200, KeyCode::BTN_TR),
        (0x0020, KeyCode::BTN_SELECT),
        (0x0010, KeyCode::BTN_START),
        (0x0400, KeyCode::BTN_MODE),
        (0x0040, KeyCode::BTN_THUMBL),
        (0x0080, KeyCode::BTN_THUMBR),
    ];

    pub fn open(b: Backend) -> Result<Box<dyn Output>, String> {
        let (vid, pid, name) = match b {
            Backend::Xinput => (0x045e, 0x028e, "Microsoft X-Box 360 pad (xidpipe)"),
            _ => (0x0000, 0x0000, "xidpipe pad"),
        };
        let mut keys = AttributeSet::<KeyCode>::new();
        KEYS.iter().for_each(|k| keys.insert(k.1));
        // fuzz/flat 0: the kernel must not add its own filtering or deadzone.
        let stick = AbsInfo::new(0, -32768, 32767, 0, 0, 0);
        let trigger = AbsInfo::new(0, 0, 255, 0, 0, 0);
        let hat = AbsInfo::new(0, -1, 1, 0, 0, 0);
        let axes = [
            (Abs::ABS_X, stick),
            (Abs::ABS_Y, stick),
            (Abs::ABS_RX, stick),
            (Abs::ABS_RY, stick),
            (Abs::ABS_Z, trigger),
            (Abs::ABS_RZ, trigger),
            (Abs::ABS_HAT0X, hat),
            (Abs::ABS_HAT0Y, hat),
        ];
        let build = || -> std::io::Result<VirtualDevice> {
            let mut d = VirtualDevice::builder()?
                .name(name)
                .input_id(InputId::new(BusType::BUS_USB, vid, pid, 0x0110))
                .with_keys(&keys)?;
            for (code, info) in axes {
                d = d.with_absolute_axis(&UinputAbsSetup::new(code, info))?;
            }
            d.build()
        };
        let dev = build().map_err(|e| {
            format!("uinput: {e} (need write access to /dev/uinput, see README: Linux permissions)")
        })?;
        Ok(Box::new(Uinput(dev)))
    }

    struct Uinput(VirtualDevice);
    impl Output for Uinput {
        fn send(&mut self, o: &Out) -> Result<(), String> {
            let abs = |c: Abs, v: i32| InputEvent::new(EventType::ABSOLUTE.0, c.0, v);
            let on = |bit: u16| o.buttons & bit != 0;
            let mut ev: Vec<InputEvent> = KEYS
                .iter()
                .map(|k| InputEvent::new(EventType::KEY.0, k.1.0, on(k.0) as i32))
                .collect();
            let [lx, ly, rx, ry] = o.sticks;
            ev.extend([
                abs(Abs::ABS_X, lx as i32),
                abs(Abs::ABS_Y, !ly as i32), // evdev +Y is down; !v maps 32767 -> -32768 exactly
                abs(Abs::ABS_RX, rx as i32),
                abs(Abs::ABS_RY, !ry as i32),
                abs(Abs::ABS_Z, o.lt as i32),
                abs(Abs::ABS_RZ, o.rt as i32),
                abs(Abs::ABS_HAT0X, on(0x0008) as i32 - on(0x0004) as i32),
                abs(Abs::ABS_HAT0Y, on(0x0002) as i32 - on(0x0001) as i32),
            ]);
            self.0.emit(&ev).map_err(|e| e.to_string())
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;
    pub fn open(_: Backend) -> Result<Box<dyn Output>, String> {
        Err(
            "no virtual controller support on this OS yet; use backend = \"none\" with --monitor"
                .into(),
        )
    }
}
