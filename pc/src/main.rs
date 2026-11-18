// SPDX-License-Identifier: GPL-3.0-or-later
mod engine;
// Tests don't need the window, and leaving it out keeps GUI system libraries out of the test binary.
#[cfg(not(test))]
mod gui;
mod output;
mod pad;
#[cfg(windows)]
mod vjoy;

use pad::{Preset, Profile};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::{env, fs};

pub const BUILTIN: [(&str, &str); 2] = [
    ("xbox", include_str!("../presets/xbox.toml")),
    ("zero", include_str!("../presets/zero.toml")),
];

const USAGE: &str = "\
usage:
  xidpipe                                     open the window (presets, live view)
  xidpipe PRESET [--xbox IP] [--poll HZ] [--monitor]   run without a window
  xidpipe save NAME [FROM]                    copy preset FROM (default: xbox) to NAME for editing
  xidpipe list                                list presets
  xidpipe --version                           show the version

PRESET is a built-in name (xbox, zero), a saved preset name, or a path to a .toml file.
--xbox IP   talk to this Xbox directly instead of finding it by broadcast
--poll HZ   poll the controllers at 125, 250, 500 or 1000 Hz (default: auto, the
            controller's own rate but at least 125 Hz)
--monitor   print controller input and output, for checking deadzones and mapping";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    #[cfg(not(test))]
    let result = if args.is_empty() {
        gui::run()
    } else {
        run(args)
    };
    #[cfg(test)]
    let result = run(args);
    match result {
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
    let mut poll_hz = 0;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "save" => return save(it.next().ok_or(USAGE)?, it.next().map_or("xbox", |s| s)),
            "list" => return list(),
            "--xbox" => xbox = Some(parse_ip(it.next().ok_or(USAGE)?)?),
            "--monitor" => monitor = true,
            "--poll" => {
                let hz = it.next().ok_or(USAGE)?;
                poll_hz = hz.parse().map_err(|_| format!("bad polling rate '{hz}'"))?;
            }
            "-h" | "--help" => return Err(USAGE.into()),
            "-V" | "--version" => {
                println!("xidpipe {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'\n{USAGE}")),
            s => preset = s.to_string(),
        }
    }
    let profile =
        Profile::new(&load_preset(&preset)?).map_err(|e| format!("preset '{preset}': {e}"))?;
    println!("xidpipe: Ctrl+C to quit");
    // The sender stays alive here so the engine never sees a closed channel.
    let (_keep, commands) = mpsc::channel();
    let hint = load_app_settings().last_xbox.parse().ok();
    let settings = engine::Settings {
        hint,
        profile,
        xbox,
        poll_hz,
    };
    engine::run(settings, commands, None, monitor)
}

pub fn parse_ip(s: &str) -> Result<IpAddr, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("bad IP address '{s}'"))
}

/// xidpipe's folder in the user's config directory.
fn config_dir() -> Option<PathBuf> {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
    };
    base.map(|b| b.join("xidpipe"))
}

pub fn preset_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("presets"))
}

/// What the window remembers between runs, in settings.toml next to the presets folder.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default)]
#[cfg_attr(test, allow(dead_code))]
pub struct AppSettings {
    /// Preset to open with ("" = xbox)
    pub preset: String,
    /// Xbox address typed by the user ("" = find it automatically)
    pub xbox: String,
    pub poll_hz: u16,
    /// Where the Xbox was found last time, tried first when searching
    pub last_xbox: String,
}

/// Missing or unreadable settings just mean defaults; they must never stop xidpipe starting.
pub fn load_app_settings() -> AppSettings {
    config_dir()
        .and_then(|d| fs::read_to_string(d.join("settings.toml")).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

#[cfg_attr(test, allow(dead_code))]
pub fn save_app_settings(s: &AppSettings) -> Result<(), String> {
    let dir = config_dir().ok_or("can't find a config directory (HOME/APPDATA not set)")?;
    let text = toml::to_string(s).map_err(|e| e.to_string())?;
    let file = dir.join("settings.toml");
    fs::create_dir_all(&dir)
        .and_then(|_| fs::write(&file, text))
        .map_err(|e| format!("{}: {e}", file.display()))
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

pub fn load_preset(name: &str) -> Result<Preset, String> {
    toml::from_str(&preset_text(name)?).map_err(|e| format!("preset '{name}': {e}"))
}

pub fn is_builtin(name: &str) -> bool {
    BUILTIN.iter().any(|b| b.0 == name)
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("preset names may only use letters, digits, '-' and '_'".into());
    }
    if is_builtin(name) {
        return Err(format!("'{name}' is a built-in preset; pick another name"));
    }
    Ok(())
}

/// Write a saved preset. `overwrite` false refuses to replace an existing file.
pub fn write_preset(name: &str, text: &str, overwrite: bool) -> Result<PathBuf, String> {
    check_name(name)?;
    let dir = preset_dir().ok_or("can't find a config directory (HOME/APPDATA not set)")?;
    let file = dir.join(format!("{name}.toml"));
    if !overwrite && file.exists() {
        return Err(format!("{} already exists", file.display()));
    }
    fs::create_dir_all(&dir)
        .and_then(|_| fs::write(&file, text))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

fn save(name: &str, from: &str) -> Result<(), String> {
    let file = write_preset(name, &preset_text(from)?, false)?;
    println!(
        "saved {}\nedit it, then run: xidpipe {name}",
        file.display()
    );
    Ok(())
}

pub fn saved_presets() -> Vec<String> {
    let mut saved: Vec<String> = preset_dir()
        .and_then(|d| fs::read_dir(d).ok())
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
    saved
}

fn list() -> Result<(), String> {
    println!("built-in: {}", BUILTIN.map(|b| b.0).join(", "));
    let saved = saved_presets();
    let dir = preset_dir().map_or("?".into(), |d| d.display().to_string());
    let names = if saved.is_empty() {
        "none".into()
    } else {
        saved.join(", ")
    };
    println!("saved ({dir}): {names}");
    Ok(())
}
