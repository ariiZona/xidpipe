// SPDX-License-Identifier: GPL-3.0-or-later
mod output;
mod pad;

use pad::{Out, Preset, Profile};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use std::{env, fs};

/// UDP port the Xbox app listens on. Keep in sync with xbox/main.c.
const XBOX_PORT: u16 = 47100;
const MAGIC: &[u8; 2] = b"XP";
const VERSION: u8 = 1;
const MSG_STATE: u8 = 1;
const MSG_UNPLUGGED: u8 = 2;
const MSG_HELLO: u8 = 0x10;
const HEADER: usize = 14;
/// The Xbox resends every 50 ms; this many ms of silence means the link is gone.
const LINK_TIMEOUT: Duration = Duration::from_millis(500);
const HELLO_EVERY: Duration = Duration::from_secs(1);
const MAX_PADS: u8 = 12;

pub const BUILTIN: [(&str, &str); 2] = [
    ("xbox", include_str!("../presets/xbox.toml")),
    ("zero", include_str!("../presets/zero.toml")),
];

const USAGE: &str = "\
usage:
  xidpipe [PRESET] [--xbox IP] [--monitor]   run (default preset: xbox)
  xidpipe save NAME [FROM]                   copy preset FROM (default: xbox) to NAME for editing
  xidpipe list                               list presets

PRESET is a built-in name (xbox, zero), a saved preset name, or a path to a .toml file.
--xbox IP   talk to this Xbox directly instead of finding it by broadcast
--monitor   print controller input and output, for checking deadzones and mapping";

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xidpipe: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut preset = "xbox".to_string();
    let mut xbox = None;
    let mut monitor = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "save" => return save(it.next().ok_or(USAGE)?, it.next().map_or("xbox", |s| s)),
            "list" => return list(),
            "--xbox" => {
                let ip = it.next().ok_or(USAGE)?;
                xbox = Some(
                    ip.parse::<IpAddr>()
                        .map_err(|_| format!("bad IP address '{ip}'"))?,
                );
            }
            "--monitor" => monitor = true,
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'\n{USAGE}")),
            s => preset = s.to_string(),
        }
    }
    let text = preset_text(&preset)?;
    let parsed: Preset = toml::from_str(&text).map_err(|e| format!("preset '{preset}': {e}"))?;
    let profile = Profile::new(&parsed).map_err(|e| format!("preset '{preset}': {e}"))?;
    serve(&profile, xbox, monitor)
}

struct Slot {
    out: Box<dyn output::Output>,
    last: Out,
    seq: u32,
    rx: Instant,
    linked: bool,
    lost: u64,
}

fn serve(profile: &Profile, xbox: Option<IpAddr>, monitor: bool) -> Result<(), String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("bind: {e}"))?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    // Short timeout only paces hellos and link checks; packets wake recv immediately.
    sock.set_read_timeout(Some(Duration::from_millis(20)))
        .map_err(|e| e.to_string())?;
    let dest = SocketAddr::new(xbox.unwrap_or(IpAddr::V4(Ipv4Addr::BROADCAST)), XBOX_PORT);
    println!(
        "xidpipe: backend {:?}, looking for the Xbox at {dest} (Ctrl+C to quit)",
        profile.backend
    );

    let hello = [MAGIC[0], MAGIC[1], VERSION, MSG_HELLO];
    let mut last_hello = Instant::now() - HELLO_EVERY;
    let mut last_print = Instant::now();
    let mut peer: Option<SocketAddr> = None;
    let mut slots: HashMap<u8, Slot> = HashMap::new();
    let mut buf = [0u8; 512];

    loop {
        if last_hello.elapsed() >= HELLO_EVERY {
            // Errors here (no route yet, etc.) are retried next second.
            let _ = sock.send_to(&hello, dest);
            last_hello = Instant::now();
        }

        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if xbox.is_some_and(|ip| ip != from.ip()) {
                    continue;
                }
                let Some(msg) = decode(&buf[..n]) else {
                    continue;
                };
                if peer != Some(from) {
                    println!("xidpipe: Xbox at {from}");
                    peer = Some(from);
                }
                match msg {
                    Msg::Unplugged { pad } => {
                        if let Some(s) = slots.get_mut(&pad) {
                            println!("xidpipe: pad {pad} unplugged");
                            neutral(pad, s);
                        }
                    }
                    Msg::State {
                        pad,
                        seq,
                        vid,
                        pid,
                        report,
                    } => {
                        let s = match slots.get_mut(&pad) {
                            Some(s) => s,
                            None => {
                                let out = output::open(profile.backend)?;
                                println!("xidpipe: pad {pad} connected (USB {vid:04x}:{pid:04x})");
                                slots.entry(pad).or_insert(Slot {
                                    out,
                                    last: Out::default(),
                                    seq: seq.wrapping_sub(1),
                                    rx: Instant::now(),
                                    linked: false,
                                    lost: 0,
                                })
                            }
                        };
                        let ahead = seq.wrapping_sub(s.seq);
                        if s.linked && (ahead == 0 || ahead > u32::MAX / 2) {
                            continue; // duplicate or reordered: a newer state was already applied
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
                                continue;
                            }
                            s.last = o;
                        }
                        if monitor && last_print.elapsed() >= Duration::from_millis(100) {
                            last_print = Instant::now();
                            println!("pad {pad} in  {x:?}\n      out {o:?} lost={}", s.lost);
                        }
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

        // Never leave a button held if the network or the Xbox goes away.
        for (pad, s) in slots.iter_mut() {
            if s.linked && s.rx.elapsed() > LINK_TIMEOUT {
                println!("xidpipe: pad {pad}: no data from the Xbox, releasing all inputs");
                neutral(*pad, s);
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

fn preset_dir() -> Option<PathBuf> {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
    };
    base.map(|b| b.join("xidpipe").join("presets"))
}

fn preset_text(name: &str) -> Result<String, String> {
    let as_path = Path::new(name);
    if name.ends_with(".toml") || as_path.components().count() > 1 {
        return fs::read_to_string(as_path).map_err(|e| format!("{name}: {e}"));
    }
    if let Some(f) = preset_dir()
        .map(|d| d.join(format!("{name}.toml")))
        .filter(|f| f.exists())
    {
        return fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()));
    }
    BUILTIN
        .iter()
        .find(|b| b.0 == name)
        .map(|b| b.1.to_string())
        .ok_or(format!("unknown preset '{name}' (see: xidpipe list)"))
}

fn save(name: &str, from: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("preset names may only use letters, digits, '-' and '_'".into());
    }
    let text = preset_text(from)?;
    let dir = preset_dir().ok_or("can't find a config directory (HOME/APPDATA not set)")?;
    let file = dir.join(format!("{name}.toml"));
    if file.exists() {
        return Err(format!(
            "{} already exists; edit it directly",
            file.display()
        ));
    }
    fs::create_dir_all(&dir)
        .and_then(|_| fs::write(&file, text))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    println!(
        "saved {}\nedit it, then run: xidpipe {name}",
        file.display()
    );
    Ok(())
}

fn list() -> Result<(), String> {
    println!("built-in: {}", BUILTIN.map(|b| b.0).join(", "));
    let Some(dir) = preset_dir() else {
        return Ok(());
    };
    let mut saved: Vec<String> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".toml")
                .map(String::from)
        })
        .collect();
    saved.sort();
    println!(
        "saved ({}): {}",
        dir.display(),
        if saved.is_empty() {
            "none".into()
        } else {
            saved.join(", ")
        }
    );
    Ok(())
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
}
