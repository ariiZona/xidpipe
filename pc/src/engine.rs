// SPDX-License-Identifier: GPL-3.0-or-later
//! Network loop: Xbox packets in, virtual controller updates and rumble out.

use crate::output::{self, Output};
use crate::pad::{self, Backend, Out, Profile, Xid};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// UDP port the Xbox app listens on. Keep in sync with xbox/main.c.
pub const XBOX_PORT: u16 = 47100;
const MAGIC: &[u8; 2] = b"XP";
const VERSION: u8 = 1;
const MSG_STATE: u8 = 1;
const MSG_UNPLUGGED: u8 = 2;
const MSG_HELLO: u8 = 0x10;
const MSG_RUMBLE: u8 = 0x11;
const HEADER: usize = 14;
/// The Xbox resends every 50 ms; this much silence means the link is gone.
const LINK_TIMEOUT: Duration = Duration::from_millis(500);
const HELLO_EVERY: Duration = Duration::from_secs(1);
/// Rumble is resent this often, so a lost packet can't leave a motor on or off.
/// The Xbox stops the motors by itself after 1 s without one.
const RUMBLE_EVERY: Duration = Duration::from_millis(250);
const REOPEN_AFTER: Duration = Duration::from_secs(2);
const MAX_PADS: u8 = 12;

pub struct Settings {
    pub profile: Profile,
    pub xbox: Option<IpAddr>,
}

/// What the GUI shows about one pad.
#[derive(Clone, Default)]
#[cfg_attr(test, allow(dead_code))]
pub struct PadView {
    pub vid: u16,
    pub pid: u16,
    pub raw: Xid,
    pub out: Out,
    pub lost: u64,
    pub linked: bool,
    pub rumble: (u16, u16),
    pub error: Option<String>,
}

#[derive(Default)]
#[cfg_attr(test, allow(dead_code))]
pub struct Status {
    pub xbox: Option<SocketAddr>,
    pub pads: BTreeMap<u8, PadView>,
    pub error: Option<String>,
}

struct Slot {
    out: Box<dyn Output>,
    last: Out,
    seq: u32,
    rx: Instant,
    linked: bool,
    lost: u64,
    rumble: (u16, u16),
    rumble_at: Instant,
}

/// Changing any of these needs new virtual controllers; everything else applies live.
fn output_key(p: &Profile) -> (Backend, u8) {
    (p.backend, p.vjoy_device)
}

/// Runs until the process ends. `updates` swaps settings live; `status` (GUI) gets live state.
pub fn run(
    mut settings: Settings,
    updates: Receiver<Settings>,
    status: Option<Arc<Mutex<Status>>>,
    monitor: bool,
) -> Result<(), String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("bind: {e}"))?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    // Short timeout only paces hellos, rumble polling and link checks; packets wake recv at once.
    sock.set_read_timeout(Some(Duration::from_millis(10)))
        .map_err(|e| e.to_string())?;
    let dest = |s: &Settings| {
        SocketAddr::new(s.xbox.unwrap_or(IpAddr::V4(Ipv4Addr::BROADCAST)), XBOX_PORT)
    };
    println!(
        "xidpipe: backend {:?}, looking for the Xbox at {}",
        settings.profile.backend,
        dest(&settings)
    );

    let hello = [MAGIC[0], MAGIC[1], VERSION, MSG_HELLO];
    let mut last_hello = Instant::now() - HELLO_EVERY;
    let mut last_print = Instant::now();
    let mut peer: Option<SocketAddr> = None;
    let mut slots: HashMap<u8, Slot> = HashMap::new();
    let mut open_failed: HashMap<u8, Instant> = HashMap::new();
    let mut buf = [0u8; 512];
    let show = |f: &mut dyn FnMut(&mut Status)| {
        if let Some(s) = &status {
            f(&mut s.lock().unwrap());
        }
    };

    loop {
        if let Some(new) = updates.try_iter().last() {
            if output_key(&new.profile) != output_key(&settings.profile) {
                slots.clear(); // dropping them removes the old virtual controllers
                open_failed.clear();
            }
            if new.xbox != settings.xbox {
                peer = None;
                last_hello = Instant::now() - HELLO_EVERY;
            }
            settings = new;
        }
        let profile = &settings.profile;

        if last_hello.elapsed() >= HELLO_EVERY {
            // Errors here (no route yet, etc.) are retried next second.
            let _ = sock.send_to(&hello, dest(&settings));
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
                if peer != Some(from) {
                    println!("xidpipe: Xbox at {from}");
                    peer = Some(from);
                    show(&mut |s| s.xbox = Some(from));
                }
                match msg {
                    Msg::Unplugged { pad } => {
                        if let Some(s) = slots.get_mut(&pad) {
                            println!("xidpipe: pad {pad} unplugged");
                            neutral(pad, s);
                        }
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
                                match output::open(profile, pad) {
                                    Ok(out) => {
                                        println!(
                                            "xidpipe: pad {pad} connected (USB {vid:04x}:{pid:04x})"
                                        );
                                        open_failed.remove(&pad);
                                        v.insert(Slot {
                                            out,
                                            last: Out::default(),
                                            seq: seq.wrapping_sub(1),
                                            rx: Instant::now(),
                                            linked: false,
                                            lost: 0,
                                            rumble: (0, 0),
                                            rumble_at: Instant::now(),
                                        })
                                    }
                                    Err(e) => {
                                        eprintln!("xidpipe: pad {pad}: {e}");
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
                        if s.linked {
                            s.lost += (ahead - 1) as u64;
                        }
                        s.seq = seq;
                        s.rx = Instant::now();
                        s.linked = true;
                        let x = pad::parse(report);
                        let o = profile.apply(&x);
                        if o != s.last {
                            if let Err(e) = s.out.send(&o) {
                                eprintln!("xidpipe: pad {pad}: output failed ({e}), recreating it");
                                slots.remove(&pad);
                                break 'packet;
                            }
                            s.last = o;
                        }
                        if monitor && last_print.elapsed() >= Duration::from_millis(100) {
                            last_print = Instant::now();
                            println!("pad {pad} in  {x:?}\n      out {o:?} lost={}", s.lost);
                        }
                        let (lost, rumble) = (s.lost, s.rumble);
                        show(&mut |st| {
                            st.pads.insert(
                                pad,
                                PadView {
                                    vid,
                                    pid,
                                    raw: x,
                                    out: o,
                                    lost,
                                    linked: true,
                                    rumble,
                                    error: None,
                                },
                            );
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
            Err(e) => eprintln!("xidpipe: recv: {e}"),
        }

        for (&pad, s) in slots.iter_mut() {
            // Never leave a button held if the network or the Xbox goes away.
            if s.linked && s.rx.elapsed() > LINK_TIMEOUT {
                println!("xidpipe: pad {pad}: no data from the Xbox, releasing all inputs");
                neutral(pad, s);
                show(&mut |st| {
                    if let Some(v) = st.pads.get_mut(&pad) {
                        v.linked = false;
                    }
                });
            }
            let changed = s.out.rumble();
            if let Some(r) = changed {
                s.rumble = r;
            }
            if let Some(to) = peer
                && (changed.is_some() || s.rumble_at.elapsed() >= RUMBLE_EVERY)
            {
                let _ = sock.send_to(&rumble_packet(pad, s.rumble), to);
                s.rumble_at = Instant::now();
            }
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
}

/// Packet layout (little-endian), see README "Protocol":
/// magic "XP" | version | type | pad | report len | vid u16 | pid u16 | seq u32 | report
fn decode(b: &[u8]) -> Option<Msg<'_>> {
    if b.len() < HEADER || &b[..2] != MAGIC || b[2] != VERSION {
        return None;
    }
    let pad = b[4];
    let len = b[5] as usize;
    if pad == 0 || pad > MAX_PADS || len > 32 || b.len() < HEADER + len {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    match b[3] {
        MSG_STATE => Some(Msg::State {
            pad,
            vid: u16_at(6),
            pid: u16_at(8),
            seq: u32::from_le_bytes(b[10..14].try_into().unwrap()),
            report: &b[HEADER..HEADER + len],
        }),
        MSG_UNPLUGGED => Some(Msg::Unplugged { pad }),
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
    }

    #[test]
    fn encodes_rumble() {
        assert_eq!(
            rumble_packet(3, (0x1234, 0xffff)),
            [b'X', b'P', VERSION, MSG_RUMBLE, 3, 0x34, 0x12, 0xff, 0xff]
        );
    }
}
