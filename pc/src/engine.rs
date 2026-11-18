// SPDX-License-Identifier: GPL-3.0-or-later
//! Network loop: Xbox packets in, virtual controller updates out. Rumble goes the other way
//! through [`Rumble`], straight from the backend's own thread, so it never waits on this loop.

use crate::output::{self, Output};
use crate::pad::{self, Backend, Out, Profile, Xid};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// UDP port the Xbox app listens on. Keep in sync with xbox/main.c.
pub const XBOX_PORT: u16 = 47100;
const MAGIC: &[u8; 2] = b"XP";
const VERSION: u8 = 1;
const MSG_STATE: u8 = 1;
const MSG_UNPLUGGED: u8 = 2;
const MSG_PONG: u8 = 3;
const MSG_INFO: u8 = 4;
const MSG_HELLO: u8 = 0x10;
const MSG_RUMBLE: u8 = 0x11;
const HEADER: usize = 14;
/// The Xbox resends every 50 ms; this much silence means the link is gone.
const LINK_TIMEOUT: Duration = Duration::from_millis(500);
const HELLO_EVERY: Duration = Duration::from_secs(1);
/// After this long without packets from the Xbox, search for it again instead of only
/// talking to the address it last used.
const PEER_FRESH: Duration = Duration::from_secs(3);
/// Rumble is resent this often, so a lost packet can't leave a motor on or off.
/// The Xbox stops the motors by itself after 1 s without one.
const RUMBLE_EVERY: Duration = Duration::from_millis(250);
const REOPEN_AFTER: Duration = Duration::from_secs(2);
const MAX_PADS: u8 = 12;
const LOG_LINES: usize = 300;

pub struct Settings {
    pub profile: Profile,
    pub xbox: Option<IpAddr>,
    /// Controller USB polling rate asked of the Xbox; 0 = auto (controller's own, at least 125 Hz).
    pub poll_hz: u16,
    /// Where the Xbox was found last time; tried directly while searching.
    pub hint: Option<IpAddr>,
}

#[cfg_attr(test, allow(dead_code))] // only the window sends commands
pub enum Command {
    Settings(Settings),
    /// Run the motors of `pad` for a moment, to check the rumble path without a game.
    TestRumble {
        pad: u8,
        level: (u16, u16),
        time: Duration,
    },
}

/// What the Xbox reports about a pad once a second.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct XboxInfo {
    /// Polling interval the controller asks for, and the one used (ms)
    pub dev_interval: u8,
    pub interval: u8,
    /// XID type/subtype; 0/0 = the pad had no XID descriptor
    pub xid: (u8, u8),
    pub has_rumble: bool,
    pub reports_per_s: u16,
    pub usb_errors: u16,
    /// Longest gap between two controller reports in the last second, timed on the Xbox
    /// before the network (None from older Xbox builds)
    pub worst_gap: Option<Duration>,
}

/// What the GUI shows about one pad.
#[derive(Clone, Default)]
#[cfg_attr(test, allow(dead_code))]
pub struct PadView {
    pub vid: u16,
    pub pid: u16,
    pub raw: Xid,
    pub report: Vec<u8>,
    pub out: Out,
    pub lost: u64,
    pub linked: bool,
    pub last_rx: Option<Instant>,
    /// Packets per second the PC received, and the longest gap between two (last second)
    pub rate: f32,
    pub worst_gap: Duration,
    pub info: Option<XboxInfo>,
    pub rumble: (u16, u16),
    pub error: Option<String>,
}

#[derive(Default)]
#[cfg_attr(test, allow(dead_code))]
pub struct Status {
    pub xbox: Option<SocketAddr>,
    /// Network round trip to the Xbox app: last, best, smoothed
    pub rtt: Option<(Duration, Duration, Duration)>,
    pub pads: BTreeMap<u8, PadView>,
    pub error: Option<String>,
    pub log: VecDeque<String>,
}

/// Sends rumble to the Xbox the moment a backend reports it, from any thread.
pub struct Rumble {
    sock: UdpSocket,
    xbox: Mutex<Option<IpAddr>>,
    /// What the game asked for, per pad; the strength setting is applied when sending.
    levels: Mutex<BTreeMap<u8, (u16, u16)>>,
    strength: AtomicU8,
}

impl Rumble {
    pub fn set(&self, pad: u8, level: (u16, u16)) {
        self.levels.lock().unwrap().insert(pad, level);
        self.send(pad, level);
    }

    fn send(&self, pad: u8, level: (u16, u16)) {
        if let Some(ip) = *self.xbox.lock().unwrap() {
            let level = scale_rumble(level, self.strength.load(Ordering::Relaxed));
            let _ = self
                .sock
                .send_to(&rumble_packet(pad, level), (ip, XBOX_PORT));
        }
    }

    fn resend_all(&self) {
        let all = self.levels.lock().unwrap().clone();
        for (pad, level) in all {
            self.send(pad, level);
        }
    }

    /// What the motors get now, after the strength setting.
    fn level(&self, pad: u8) -> (u16, u16) {
        let level = self
            .levels
            .lock()
            .unwrap()
            .get(&pad)
            .copied()
            .unwrap_or_default();
        scale_rumble(level, self.strength.load(Ordering::Relaxed))
    }
}

/// Vibration strength: `percent` of each motor's level, so 100 leaves it unchanged and 0 turns it off.
fn scale_rumble((l, r): (u16, u16), percent: u8) -> (u16, u16) {
    let f = |v: u16| (v as u32 * percent.min(100) as u32 / 100) as u16;
    (f(l), f(r))
}

struct Slot {
    out: Box<dyn Output>,
    last: Out,
    seq: u32,
    rx: Instant,
    linked: bool,
    lost: u64,
    // Rate measurement over the current one-second window
    window_start: Instant,
    window_count: u32,
    window_gap: Duration,
    rate: f32,
    worst_gap: Duration,
}

/// Changing any of these needs new virtual controllers; everything else applies live.
fn output_key(p: &Profile) -> (Backend, u8) {
    (p.backend, p.vjoy_device)
}

/// USB polling interval (ms) for a requested rate: the slowest supported interval that is
/// at least as fast as asked. 0 stays 0 (auto).
pub fn poll_ms(hz: u16) -> u8 {
    if hz == 0 {
        return 0;
    }
    let ms = 1000 / hz.max(1);
    [8u8, 4, 2, 1]
        .into_iter()
        .find(|&m| m as u16 <= ms)
        .unwrap_or(1)
}

/// Runs until the process ends. `commands` changes settings live; `status` (GUI) gets live state.
pub fn run(
    mut settings: Settings,
    commands: Receiver<Command>,
    status: Option<Arc<Mutex<Status>>>,
    monitor: bool,
) -> Result<(), String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("bind: {e}"))?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    // Short timeout only paces hellos, resends and link checks; packets wake recv at once.
    sock.set_read_timeout(Some(Duration::from_millis(10)))
        .map_err(|e| e.to_string())?;
    let rumble = Arc::new(Rumble {
        sock: sock.try_clone().map_err(|e| e.to_string())?,
        xbox: Mutex::new(None),
        levels: Mutex::new(BTreeMap::new()),
        strength: AtomicU8::new(settings.profile.rumble_strength),
    });
    let show = |f: &mut dyn FnMut(&mut Status)| {
        if let Some(s) = &status {
            f(&mut s.lock().unwrap());
        }
    };
    let say = |msg: String| {
        println!("xidpipe: {msg}");
        show(&mut |s| {
            if s.log.len() == LOG_LINES {
                s.log.pop_front();
            }
            s.log.push_back(msg.clone());
        });
    };
    say(format!("backend {:?}", settings.profile.backend));

    let start = Instant::now();
    let mut last_hello = start - HELLO_EVERY;
    let mut last_rumble = start;
    let mut last_print = start;
    let mut peer: Option<SocketAddr> = None;
    let mut peer_seen = start;
    let mut searching_via: Vec<IpAddr> = Vec::new();
    let mut slots: HashMap<u8, Slot> = HashMap::new();
    let mut open_failed: HashMap<u8, Instant> = HashMap::new();
    let mut test_stop: HashMap<u8, Instant> = HashMap::new();
    let mut rtt: Option<(Duration, Duration, Duration)> = None;
    let mut buf = [0u8; 512];

    loop {
        for cmd in commands.try_iter() {
            match cmd {
                Command::Settings(new) => {
                    if output_key(&new.profile) != output_key(&settings.profile) {
                        slots.clear(); // dropping them removes the old virtual controllers
                        open_failed.clear();
                    }
                    if new.xbox != settings.xbox {
                        peer = None;
                        *rumble.xbox.lock().unwrap() = None;
                    }
                    if new.xbox != settings.xbox || new.poll_hz != settings.poll_hz {
                        last_hello = start - HELLO_EVERY; // tell the Xbox right away
                    }
                    let strength = new.profile.rumble_strength;
                    if rumble.strength.swap(strength, Ordering::Relaxed) != strength {
                        rumble.resend_all(); // a running rumble changes strength at once
                    }
                    settings = new;
                }
                Command::TestRumble { pad, level, time } => {
                    rumble.set(pad, level);
                    test_stop.insert(pad, Instant::now() + time);
                }
            }
        }
        let profile = &settings.profile;

        if last_hello.elapsed() >= HELLO_EVERY {
            // Errors here (no route yet, etc.) are retried next second.
            let ping = start.elapsed().as_micros() as u32;
            let hello = hello_packet(poll_ms(settings.poll_hz), ping);
            let live = peer
                .filter(|_| peer_seen.elapsed() < PEER_FRESH)
                .map(|p| p.ip());
            let targets = hello_targets(settings.xbox, live, settings.hint, &local_ipv4s());
            if live.is_none() && targets != searching_via {
                let list: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
                say(format!("looking for the Xbox via {}", list.join(", ")));
                searching_via = targets.clone();
            }
            for ip in targets {
                let _ = sock.send_to(&hello, (ip, XBOX_PORT));
            }
            last_hello = Instant::now();
        }

        match sock.recv_from(&mut buf) {
            Ok((n, from)) => 'packet: {
                if settings.xbox.is_some_and(|ip| ip != from.ip()) {
                    break 'packet;
                }
                let Some(msg) = decode(&buf[..n]) else {
                    break 'packet;
                };
                peer_seen = Instant::now();
                if peer != Some(from) {
                    say(format!("Xbox at {from}"));
                    searching_via.clear();
                    peer = Some(from);
                    *rumble.xbox.lock().unwrap() = Some(from.ip());
                    show(&mut |s| s.xbox = Some(from));
                }
                match msg {
                    Msg::Pong { ping } => {
                        let now = start.elapsed().as_micros() as u32;
                        let t = Duration::from_micros(now.wrapping_sub(ping) as u64);
                        rtt = Some(match rtt {
                            None => (t, t, t),
                            Some((_, best, avg)) => (t, best.min(t), (avg * 7 + t) / 8),
                        });
                        show(&mut |s| s.rtt = rtt);
                    }
                    Msg::Info { pad, info } => show(&mut |s| {
                        if let Some(v) = s.pads.get_mut(&pad) {
                            v.info = Some(info);
                        }
                    }),
                    Msg::Unplugged { pad } => {
                        if let Some(s) = slots.get_mut(&pad) {
                            say(format!("pad {pad} unplugged"));
                            neutral(pad, s);
                        }
                        rumble.set(pad, (0, 0));
                        show(&mut |s| {
                            s.pads.remove(&pad);
                        });
                    }
                    Msg::State {
                        pad,
                        seq,
                        vid,
                        pid,
                        report,
                    } => {
                        let s = match slots.entry(pad) {
                            Entry::Occupied(e) => e.into_mut(),
                            Entry::Vacant(v) => {
                                if open_failed
                                    .get(&pad)
                                    .is_some_and(|t| t.elapsed() < REOPEN_AFTER)
                                {
                                    break 'packet;
                                }
                                match output::open(profile, pad, &rumble) {
                                    Ok(out) => {
                                        say(format!(
                                            "pad {pad} connected (USB {vid:04x}:{pid:04x})"
                                        ));
                                        open_failed.remove(&pad);
                                        let now = Instant::now();
                                        v.insert(Slot {
                                            out,
                                            last: Out::default(),
                                            seq: seq.wrapping_sub(1),
                                            rx: now,
                                            linked: false,
                                            lost: 0,
                                            window_start: now,
                                            window_count: 0,
                                            window_gap: Duration::ZERO,
                                            rate: 0.0,
                                            worst_gap: Duration::ZERO,
                                        })
                                    }
                                    Err(e) => {
                                        say(format!("pad {pad}: {e}"));
                                        open_failed.insert(pad, Instant::now());
                                        show(&mut |s| {
                                            s.pads.entry(pad).or_default().error = Some(e.clone())
                                        });
                                        break 'packet;
                                    }
                                }
                            }
                        };
                        let ahead = seq.wrapping_sub(s.seq);
                        if s.linked && (ahead == 0 || ahead > u32::MAX / 2) {
                            break 'packet; // duplicate or reordered: a newer state was already applied
                        }
                        let now = Instant::now();
                        if s.linked {
                            s.lost += (ahead - 1) as u64;
                            s.window_gap = s.window_gap.max(now - s.rx);
                        }
                        s.window_count += 1;
                        let window = now - s.window_start;
                        if window >= Duration::from_secs(1) {
                            s.rate = s.window_count as f32 / window.as_secs_f32();
                            s.worst_gap = s.window_gap;
                            s.window_start = now;
                            s.window_count = 0;
                            s.window_gap = Duration::ZERO;
                        }
                        s.seq = seq;
                        s.rx = now;
                        s.linked = true;
                        let x = pad::parse(report);
                        let o = profile.apply(&x);
                        if o != s.last {
                            if let Err(e) = s.out.send(&o) {
                                say(format!("pad {pad}: output failed ({e}), recreating it"));
                                slots.remove(&pad);
                                break 'packet;
                            }
                            s.last = o;
                        }
                        if monitor && last_print.elapsed() >= Duration::from_millis(100) {
                            last_print = Instant::now();
                            println!("pad {pad} in  {x:?}\n      out {o:?} lost={}", s.lost);
                        }
                        let level = rumble.level(pad);
                        let (lost, rate, worst_gap) = (s.lost, s.rate, s.worst_gap);
                        show(&mut |st| {
                            let v = st.pads.entry(pad).or_default();
                            let info = v.info;
                            *v = PadView {
                                vid,
                                pid,
                                raw: x,
                                report: report.to_vec(),
                                out: o,
                                lost,
                                linked: true,
                                last_rx: Some(now),
                                rate,
                                worst_gap,
                                info,
                                rumble: level,
                                error: None,
                            };
                        });
                    }
                }
            }
            // Timeouts are normal. Windows also reports ICMP "port unreachable" from an
            // earlier send as ConnectionReset on recv; that just means the Xbox app isn't up yet.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                ) => {}
            Err(e) => say(format!("recv: {e}")),
        }

        // Never leave a button held if the network or the Xbox goes away.
        for (&pad, s) in slots.iter_mut() {
            if s.linked && s.rx.elapsed() > LINK_TIMEOUT {
                say(format!(
                    "pad {pad}: no data from the Xbox, releasing all inputs"
                ));
                neutral(pad, s);
                show(&mut |st| {
                    if let Some(v) = st.pads.get_mut(&pad) {
                        v.linked = false;
                    }
                });
            }
        }
        test_stop.retain(|&pad, until| {
            let running = Instant::now() < *until;
            if !running {
                rumble.set(pad, (0, 0));
            }
            running
        });
        if last_rumble.elapsed() >= RUMBLE_EVERY {
            rumble.resend_all();
            last_rumble = Instant::now();
        }
    }
}

fn neutral(pad: u8, s: &mut Slot) {
    s.linked = false;
    s.last = Out::default();
    if let Err(e) = s.out.send(&s.last) {
        eprintln!("xidpipe: pad {pad}: {e}");
    }
}

/// Where to send the hello. A manual address wins; a live Xbox gets unicast only. Otherwise
/// search: the last known address, the limited broadcast, and each local network's own
/// broadcast address. Windows sends 255.255.255.255 out of only one adapter, often the wrong
/// one on PCs with VPN, WSL/Hyper-V or VirtualBox adapters; subnet broadcasts always go out
/// of the adapter they belong to.
fn hello_targets(
    manual: Option<IpAddr>,
    live_peer: Option<IpAddr>,
    hint: Option<IpAddr>,
    local: &[(Ipv4Addr, Ipv4Addr)],
) -> Vec<IpAddr> {
    if let Some(ip) = manual.or(live_peer) {
        return vec![ip];
    }
    let mut out: Vec<IpAddr> = hint.into_iter().collect();
    out.push(IpAddr::V4(Ipv4Addr::BROADCAST));
    for &(ip, mask) in local {
        // Skip loopback and point-to-point links (VPNs often use /32), which have no broadcast.
        if ip.is_loopback() || u32::from(mask) >= 0xFFFF_FFFE {
            continue;
        }
        let bcast = IpAddr::V4(Ipv4Addr::from(u32::from(ip) | !u32::from(mask)));
        if !out.contains(&bcast) {
            out.push(bcast);
        }
    }
    out
}

/// This PC's IPv4 addresses with their netmasks (empty if they can't be read).
fn local_ipv4s() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) => Some((a.ip, a.netmask)),
            _ => None,
        })
        .collect()
}

/// PC -> Xbox: magic | version | type | poll ms (0 = auto) | 0 | ping u32 (echoed in a pong)
fn hello_packet(poll_ms: u8, ping: u32) -> [u8; 10] {
    let p = ping.to_le_bytes();
    [
        MAGIC[0], MAGIC[1], VERSION, MSG_HELLO, poll_ms, 0, p[0], p[1], p[2], p[3],
    ]
}

/// PC -> Xbox: magic | version | type | pad | left motor u16 | right motor u16
fn rumble_packet(pad: u8, (l, r): (u16, u16)) -> [u8; 9] {
    let (l, r) = (l.to_le_bytes(), r.to_le_bytes());
    [
        MAGIC[0], MAGIC[1], VERSION, MSG_RUMBLE, pad, l[0], l[1], r[0], r[1],
    ]
}

#[derive(Debug, PartialEq)]
enum Msg<'a> {
    State {
        pad: u8,
        seq: u32,
        vid: u16,
        pid: u16,
        report: &'a [u8],
    },
    Unplugged {
        pad: u8,
    },
    Pong {
        ping: u32,
    },
    Info {
        pad: u8,
        info: XboxInfo,
    },
}

/// Xbox -> PC packets (little-endian), see README "Protocol".
fn decode(b: &[u8]) -> Option<Msg<'_>> {
    if b.len() < 4 || &b[..2] != MAGIC || b[2] != VERSION {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    match b[3] {
        MSG_PONG if b.len() >= 10 => Some(Msg::Pong { ping: u32_at(6) }),
        MSG_INFO if b.len() >= 14 && (1..=MAX_PADS).contains(&b[4]) => Some(Msg::Info {
            pad: b[4],
            info: XboxInfo {
                dev_interval: b[5],
                interval: b[6],
                xid: (b[7], b[8]),
                has_rumble: b[9] != 0,
                reports_per_s: u16_at(10),
                usb_errors: u16_at(12),
                worst_gap: (b.len() >= 16).then(|| Duration::from_micros(u16_at(14) as u64 * 100)),
            },
        }),
        MSG_STATE | MSG_UNPLUGGED => {
            // magic "XP" | version | type | pad | report len | vid u16 | pid u16 | seq u32 | report
            let (pad, len) = (b[4], *b.get(5)? as usize);
            if b.len() < HEADER || pad == 0 || pad > MAX_PADS || len > 32 || b.len() < HEADER + len
            {
                return None;
            }
            Some(if b[3] == MSG_STATE {
                Msg::State {
                    pad,
                    vid: u16_at(6),
                    pid: u16_at(8),
                    seq: u32_at(10),
                    report: &b[HEADER..HEADER + len],
                }
            } else {
                Msg::Unplugged { pad }
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_packets() {
        let mut p = vec![
            b'X', b'P', VERSION, MSG_STATE, 1, 3, 0x5e, 0x04, 0x02, 0x02, 7, 0, 0, 0, 0, 20, 1,
        ];
        assert_eq!(
            decode(&p),
            Some(Msg::State {
                pad: 1,
                seq: 7,
                vid: 0x045e,
                pid: 0x0202,
                report: &[0, 20, 1]
            })
        );
        p[5] = 4; // claims more report bytes than were sent
        assert_eq!(decode(&p), None);
        p[5] = 3;
        p[4] = 0;
        assert_eq!(decode(&p), None);
        p[4] = 2;
        p[3] = MSG_UNPLUGGED;
        assert_eq!(decode(&p), Some(Msg::Unplugged { pad: 2 }));
        assert_eq!(decode(b"XP\x02\x01"), None);
        assert_eq!(decode(b"XP\x01\x01\x01"), None); // truncated header
        assert_eq!(
            decode(&[b'X', b'P', 1, MSG_PONG, 0, 0, 1, 2, 3, 4]),
            Some(Msg::Pong { ping: 0x04030201 })
        );
        let info = [
            b'X', b'P', 1, MSG_INFO, 3, 8, 4, 1, 2, 1, 250, 0, 7, 0, 170, 0,
        ];
        let expected = XboxInfo {
            dev_interval: 8,
            interval: 4,
            xid: (1, 2),
            has_rumble: true,
            reports_per_s: 250,
            usb_errors: 7,
            worst_gap: Some(Duration::from_micros(17_000)),
        };
        assert_eq!(
            decode(&info),
            Some(Msg::Info {
                pad: 3,
                info: expected
            })
        );
        // Older Xbox builds send 14 bytes, without the gap
        let old = XboxInfo {
            worst_gap: None,
            ..expected
        };
        assert_eq!(decode(&info[..14]), Some(Msg::Info { pad: 3, info: old }));
        assert_eq!(decode(&info[..13]), None);
    }

    #[test]
    fn encodes_packets() {
        assert_eq!(
            rumble_packet(3, (0x1234, 0xffff)),
            [b'X', b'P', VERSION, MSG_RUMBLE, 3, 0x34, 0x12, 0xff, 0xff]
        );
        assert_eq!(
            hello_packet(4, 0x0a0b0c0d),
            [b'X', b'P', VERSION, MSG_HELLO, 4, 0, 0x0d, 0x0c, 0x0b, 0x0a]
        );
    }

    #[test]
    fn rumble_strength() {
        assert_eq!(scale_rumble((65535, 1000), 100), (65535, 1000));
        assert_eq!(scale_rumble((65535, 1000), 50), (32767, 500));
        assert_eq!(scale_rumble((65535, 1000), 0), (0, 0));
        assert_eq!(scale_rumble((65535, 65535), 200), (65535, 65535)); // clamped to 100%
    }

    #[test]
    fn hello_goes_to_every_network() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let v4 = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let local = [
            (v4("192.168.8.2"), v4("255.255.255.0")),
            (v4("172.30.16.1"), v4("255.255.240.0")),
            (v4("100.98.199.110"), v4("255.255.255.255")), // VPN /32: skipped
            (v4("127.0.0.1"), v4("255.0.0.0")),            // loopback: skipped
            (v4("192.168.8.9"), v4("255.255.255.0")),      // same network twice: once
        ];
        assert_eq!(
            hello_targets(None, None, Some(ip("192.168.8.50")), &local),
            [
                ip("192.168.8.50"),
                ip("255.255.255.255"),
                ip("192.168.8.255"),
                ip("172.30.31.255")
            ]
        );
        assert_eq!(
            hello_targets(None, Some(ip("192.168.8.50")), None, &local),
            [ip("192.168.8.50")]
        );
        assert_eq!(
            hello_targets(Some(ip("10.0.0.5")), Some(ip("192.168.8.50")), None, &local),
            [ip("10.0.0.5")]
        );
    }

    #[test]
    fn poll_rates() {
        assert_eq!(
            [0, 125, 250, 500, 1000, 300, 60, 5000].map(poll_ms),
            [0, 8, 4, 2, 1, 2, 8, 1]
        );
    }
}
