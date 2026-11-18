// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual controller backends.

use crate::engine::Rumble;
use crate::pad::{Backend, Out, Profile};
use std::sync::Arc;

/// A virtual controller. Backends that support rumble report it through the [`Rumble`] they
/// were opened with, from their own thread, the moment the game asks for it.
pub trait Output {
    fn send(&mut self, o: &Out) -> Result<(), String>;
}

struct Null;
impl Output for Null {
    fn send(&mut self, _: &Out) -> Result<(), String> {
        Ok(())
    }
}

/// Create the virtual controller for Xbox pad `pad` (1-12).
pub fn open(p: &Profile, pad: u8, rumble: &Arc<Rumble>) -> Result<Box<dyn Output>, String> {
    match p.backend {
        Backend::None => Ok(Box::new(Null)),
        #[cfg(windows)]
        Backend::Vjoy => crate::vjoy::open(p, pad),
        #[cfg(not(windows))]
        Backend::Vjoy => Err(format!("pad {pad}: vJoy is Windows-only")),
        b => imp::open(b, pad, rumble.clone()),
    }
}

/// Backends this OS supports, for the GUI.
pub fn available() -> Vec<Backend> {
    Backend::ALL
        .into_iter()
        .filter(|b| match b {
            Backend::None => true,
            Backend::Vjoy => cfg!(windows),
            _ => cfg!(any(windows, target_os = "linux")),
        })
        .collect()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use vigem_client::{
        Client, DS4Report, DualShock4Wired, TargetId, XButtons, XGamepad, Xbox360Wired,
    };

    const VIGEM_HELP: &str = "is ViGEmBus installed? https://github.com/nefarius/ViGEmBus/releases";

    pub fn open(b: Backend, pad: u8, rumble: Arc<Rumble>) -> Result<Box<dyn Output>, String> {
        let client = Client::connect().map_err(|e| format!("ViGEmBus: {e:?} ({VIGEM_HELP})"))?;
        if b == Backend::Dinput {
            let mut t = DualShock4Wired::new(client, TargetId::DUALSHOCK4_WIRED);
            t.plugin()
                .and_then(|_| t.wait_ready())
                .map_err(|e| format!("ViGEm DS4: {e:?}"))?;
            return Ok(Box::new(Ds4(t)));
        }
        let mut t = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
        t.plugin()
            .and_then(|_| t.wait_ready())
            .map_err(|e| format!("ViGEm X360: {e:?}"))?;
        // Rumble goes to the Xbox straight from this thread. It ends by itself when the target
        // is unplugged (dropped).
        t.request_notification()
            .map_err(|e| format!("ViGEm X360 rumble: {e:?}"))?
            .spawn_thread(move |_, n| {
                rumble.set(
                    pad,
                    (n.large_motor as u16 * 257, n.small_motor as u16 * 257),
                );
            });
        Ok(Box::new(X360 { t }))
    }

    struct X360 {
        t: Xbox360Wired<Client>,
    }
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
            self.t.update(&g).map_err(|e| format!("{e:?}"))
        }
    }

    // shortcut: no DS4 rumble; vigem-client keeps the DS4 serial number private, so its
    // notification IOCTL is unreachable. Needs a vigem-client patch or our own bus code.
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
        buttons |= crate::vjoy::hat8(o.buttons).map_or(8, u16::from);
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
            sticks: [0, 32767, -32768, 0],
            ..Out::default()
        });
        assert_eq!(r.buttons, 1 << 5 | 1 << 10 | 1);
        assert_eq!(
            (r.thumb_lx, r.thumb_ly, r.thumb_rx, r.thumb_ry),
            (128, 0, 0, 128)
        );
        assert_eq!(ds4(&Out::default()).buttons, 8);
    }
}

/// Linux force-feedback bookkeeping: which rumble effects games uploaded and which are playing.
#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct FfState {
    /// effect id -> (strong, weak magnitude, length ms; 0 = until stopped)
    effects: std::collections::HashMap<u16, (u16, u16, u16)>,
    /// effect id -> when it stops by itself
    playing: std::collections::HashMap<u16, Option<std::time::Instant>>,
}

#[cfg(any(target_os = "linux", test))]
impl FfState {
    fn upload(&mut self, id: u16, strong: u16, weak: u16, length_ms: u16) {
        self.effects.insert(id, (strong, weak, length_ms));
    }
    fn erase(&mut self, id: u16) {
        self.effects.remove(&id);
        self.playing.remove(&id);
    }
    fn play(&mut self, id: u16, on: bool, now: std::time::Instant) {
        match self.effects.get(&id) {
            Some(&(_, _, len)) if on => {
                let end = (len > 0).then(|| now + std::time::Duration::from_millis(len as u64));
                self.playing.insert(id, end);
            }
            _ => {
                self.playing.remove(&id);
            }
        }
    }
    /// Motor levels now (strongest playing effect per motor); drops finished effects.
    fn level(&mut self, now: std::time::Instant) -> (u16, u16) {
        self.playing.retain(|_, end| end.is_none_or(|e| now < e));
        self.playing
            .keys()
            .filter_map(|id| self.effects.get(id))
            .fold((0, 0), |a, e| (a.0.max(e.0), a.1.max(e.1)))
    }
    /// When the next timed effect ends, so the waiter can wake exactly then.
    fn next_end(&self) -> Option<std::time::Instant> {
        self.playing.values().flatten().min().copied()
    }
}

#[test]
fn ff_effects() {
    use std::time::{Duration, Instant};
    let t = Instant::now();
    let mut ff = FfState::default();
    ff.upload(0, 1000, 200, 0);
    ff.upload(1, 500, 900, 100);
    ff.play(0, true, t);
    ff.play(1, true, t);
    assert_eq!(ff.level(t), (1000, 900));
    assert_eq!(ff.next_end(), Some(t + Duration::from_millis(100)));
    assert_eq!(ff.level(t + Duration::from_millis(150)), (1000, 200)); // timed effect ended
    ff.play(0, false, t);
    assert_eq!(ff.level(t), (0, 0));
    ff.play(1, true, t);
    ff.erase(1);
    assert_eq!(ff.level(t), (0, 0));
    ff.play(7, true, t); // never uploaded: ignored
    assert_eq!(ff.level(t), (0, 0));
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use evdev::uinput::VirtualDevice;
    use evdev::{
        AbsInfo, AbsoluteAxisCode as Abs, AttributeSet, BusType, EventSummary, EventType,
        FFEffectCode, FFEffectKind, InputEvent, InputId, KeyCode, UInputCode, UinputAbsSetup,
    };
    use std::os::fd::{AsRawFd, RawFd};
    use std::sync::{Mutex, Weak};
    use std::time::Instant;

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

    pub fn open(b: Backend, pad: u8, rumble: Arc<Rumble>) -> Result<Box<dyn Output>, String> {
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
                .with_keys(&keys)?
                .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))?
                .with_ff_effects_max(16);
            for (code, info) in axes {
                d = d.with_absolute_axis(&UinputAbsSetup::new(code, info))?;
            }
            let dev = d.build()?;
            // The rumble thread waits with poll(); reads themselves must never block.
            let fd = dev.as_raw_fd();
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(dev)
        };
        let dev = build().map_err(|e| {
            format!("uinput: {e} (need write access to /dev/uinput, see README: Linux permissions)")
        })?;
        let fd = dev.as_raw_fd();
        let dev = Arc::new(Mutex::new(dev));
        let weak = Arc::downgrade(&dev);
        std::thread::Builder::new()
            .name(format!("rumble-{pad}"))
            .spawn(move || rumble_thread(weak, fd, pad, rumble))
            .map_err(|e| e.to_string())?;
        Ok(Box::new(Uinput(dev)))
    }

    /// Waits on the device for force-feedback requests and forwards motor levels as soon as
    /// they change. Ends when the device is dropped.
    fn rumble_thread(dev: Weak<Mutex<VirtualDevice>>, fd: RawFd, pad: u8, rumble: Arc<Rumble>) {
        let mut ff = FfState::default();
        let mut last = (0, 0);
        loop {
            // Wake on a request, when a timed effect ends, or every 250 ms to notice the device is gone.
            let wait = ff.next_end().map_or(250, |e| {
                e.saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(250) as i32
            });
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe { libc::poll(&mut pfd, 1, wait) };
            let Some(dev) = dev.upgrade() else { return };
            let mut d = dev.lock().unwrap();
            let now = Instant::now();
            // WouldBlock (nothing queued) and other read errors both just mean "no new requests".
            let events: Vec<InputEvent> =
                d.fetch_events().map(|it| it.collect()).unwrap_or_default();
            for ev in events {
                match ev.destructure() {
                    EventSummary::UInput(e, UInputCode::UI_FF_UPLOAD, ..) => {
                        if let Ok(mut up) = d.process_ff_upload(e) {
                            let data = up.effect();
                            let (s, w) = match data.kind {
                                FFEffectKind::Rumble {
                                    strong_magnitude,
                                    weak_magnitude,
                                } => (strong_magnitude, weak_magnitude),
                                _ => (0, 0),
                            };
                            // The kernel already picked the effect id; keep it.
                            ff.upload(up.effect_id() as u16, s, w, data.replay.length);
                            up.set_retval(0);
                        }
                    }
                    EventSummary::UInput(e, UInputCode::UI_FF_ERASE, ..) => {
                        if let Ok(er) = d.process_ff_erase(e) {
                            ff.erase(er.effect_id() as u16);
                        }
                    }
                    EventSummary::ForceFeedback(_, code, value) => ff.play(code.0, value > 0, now),
                    _ => {}
                }
            }
            drop(d);
            let level = ff.level(now);
            if level != last {
                last = level;
                rumble.set(pad, level);
            }
        }
    }

    struct Uinput(Arc<Mutex<VirtualDevice>>);

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
            self.0.lock().unwrap().emit(&ev).map_err(|e| e.to_string())
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;
    pub fn open(_: Backend, _: u8, _: Arc<Rumble>) -> Result<Box<dyn Output>, String> {
        Err("no virtual controller support on this OS yet; use backend = \"none\"".into())
    }
}

#[test]
fn backends_per_os() {
    let a = available();
    assert!(a.contains(&Backend::None));
    assert_eq!(a.contains(&Backend::Vjoy), cfg!(windows));
}
