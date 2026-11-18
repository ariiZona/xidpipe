# AGENTS.md

Guidance for AI coding agents (and humans) working on xidpipe. Read [README.md](README.md) first for what the
project does. This file covers how to change it without breaking it.

This repository is public. Never commit secrets, tokens, personal names or email addresses, private hostnames,
local machine paths, or links to private chat or agent sessions, in code, docs or commit messages.

## What this is

Two programs that talk over UDP:

- **`xbox/`**: the Xbox app (C, built with nxdk). It runs on a modded original Xbox, reads XID controllers over
  USB, forwards their raw reports to the PC, and drives rumble.
- **`pc/`**: the PC app (Rust). It receives reports, applies presets (deadzones, mapping, pressure), and feeds
  a virtual controller (ViGEm or vJoy on Windows, uinput on Linux). It sends rumble back. It has a window (egui)
  and a CLI.

| path | role |
|---|---|
| `xbox/main.c` | the whole Xbox app: USB probe/driver, DPC callbacks, lwIP UDP, polling, rumble |
| `xbox/Makefile` | nxdk build (`NXDK_DIR` defaults to the `xbox/nxdk` submodule) |
| `xbox/nxdk` | pinned nxdk submodule. Don't bump it casually; rebuild and retest if you do |
| `pc/src/engine.rs` | network loop, protocol encode/decode, rumble sink, live status for the GUI |
| `pc/src/pad.rs` | XID report parsing, `Preset` (TOML) / `Profile`, deadzones, snap correction, mapping, stick trace |
| `pc/src/output.rs` | backends: ViGEm X360/DS4 (Windows), uinput + force feedback (Linux), null |
| `pc/src/vjoy.rs` | vJoy backend (Windows), loaded from `vJoyInterface.dll` at run time |
| `pc/src/gui.rs` | the window: Setup, Button tester, Debug tabs |
| `pc/src/main.rs` | CLI, preset files on disk |
| `pc/presets/*.toml` | built-in presets (`xbox` documents every setting) |
| `.github/workflows/ci.yml` | CI on every push; GitHub release on `v*` tags |

## Build and check

PC app (from `pc/`):

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

CI runs exactly these on Linux, Windows and macOS, with warnings as errors. To check another OS locally
without running it: `rustup target add x86_64-unknown-linux-gnu x86_64-apple-darwin`, then
`cargo clippy --locked --all-targets --target <triple> -- -D warnings`.

Xbox app (from `xbox/`, after `git submodule update --init --recursive`):

```sh
nxdk/bin/activate make -j4      # on Windows: inside an MSYS2 MINGW64 shell
```

Builds must stay warning-free in our own code (warnings inside nxdk's tools are not ours).

## Rules that keep it working

**The protocol lives in two places.** Message types, sizes, offsets and the UDP port are defined in both
`xbox/main.c` and `pc/src/engine.rs`. Change both together, keep the README's Protocol section in sync, and
keep old packet sizes decodable where you can (see how the 14-byte vs 16-byte info packet is handled).
Add a decode/encode test in `engine.rs` for any new or changed message.

**Latency paths must not block.**
- Xbox: `on_report` and `on_rumble_done` run in the USB DPC (DISPATCH_LEVEL). Only copy, re-arm and
  `KeSetEvent` there; no sends, no prints, no waits. `on_udp` runs in lwIP's thread with the core lock
  held: don't take the lock again or block. Main-loop data shared with the DPC is copied at raised IRQL
  (single CPU); data shared with `on_udp` is touched only under `LOCK_TCPIP_CORE`.
- PC: the engine loop must not wait on the GUI or on rumble. Backends report rumble through `Rumble::set`
  from their own thread. The GUI only reads a snapshot of `Status`.

**Safety behaviour is deliberate. Don't remove it:**
- The PC releases all inputs after 500 ms without packets.
- The Xbox zeroes rumble after 1 s without rumble packets or when the PC disappears; the PC resends rumble every 250 ms.
- Only the PC the Xbox streams to may drive rumble.

**3rd-party controllers are a first-class target.** Never require an XID descriptor, specific VID/PID or
exact report length. Zero-pad short reports. Real hardware has shown pads reporting `ffff:ffff`, a fixed
~125 Hz update rate and firmware axis snapping.

**Presets are user data.** `Preset` uses `deny_unknown_fields` and validates in `Profile::new`. New fields need
a default in `Preset::default()`, a line in `pc/presets/xbox.toml` (a test checks it matches the defaults),
validation, a row in the README's settings table, and a control in the GUI if it's user-facing.

**The GUI is excluded from test builds** (`#[cfg(not(test))] mod gui`). Put testable logic in `pad.rs` or
`engine.rs`, not `gui.rs`. Items used only by the GUI need `#[cfg_attr(test, allow(dead_code))]`.

**Keep dependencies GPL-3.0-compatible** and few. Don't add a crate for a few lines of code.

## Docs, versions, releases

- The README must match what the code does. Update it in the same change, and never document features
  that don't exist or performance nobody has measured.
- The project discloses that it's AI-written ("vibecoded"). Keep that notice prominent.
- Every source file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- Release: bump `version` in `pc/Cargo.toml`, add a `## vX.Y.Z` section to `CHANGELOG.md`, commit, then
  push an annotated tag `vX.Y.Z`. CI builds everything and publishes the GitHub release; it fails if the tag
  and `Cargo.toml` disagree or the changelog section is missing.

## Testing limits

- Nothing in CI touches real hardware. CI proves it compiles and the unit tests pass, not that a controller
  works. Say so honestly in PRs and release notes, and list what was actually tested on hardware.
- The Debug tab (rates, gaps measured on both the Xbox and the PC, round trip, raw reports, rumble test) and
  the Button tester (raw input, stick trace) are the tools for diagnosing hardware reports. Ask testers for
  their numbers rather than guessing.
- Windows builds use the MSVC toolchain. Rust's GNU toolchain needs a full MinGW (`dlltool` and `as`) for the
  window's dependencies.
