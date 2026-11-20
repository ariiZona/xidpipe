# Changelog

Each release's notes on GitHub are taken from its section here.

## v0.2.6

- CI: Forgejo builds and tests the PC app and builds the Xbox app from `.forgejo/workflows/ci.yml`, on
  Linux only (no Windows runner there). GitHub keeps `.github/workflows/ci.yml`. Forgejo reads only one
  workflow folder, and `.forgejo/workflows` comes before `.github/workflows`.
- Release: a `v*` tag pushed to Forgejo now also publishes a Forgejo release with the Linux and Xbox
  builds and this section as notes. It needs a `RELEASE_TOKEN` repository secret. That release has no
  Windows build, which needs a Windows runner.
- No changes to either app: this release exists to test the Forgejo pipeline.

## v0.2.5

- Xbox: fixed rumble that could keep running until the controller was replugged, even after closing
  xidpipe on the PC. A rumble write the pad never accepted blocked every later write, "motors off"
  included. The Xbox now aborts a write the pad hasn't taken within 100 ms, and rewrites the motors
  every 250 ms while they run or after a failed write.
- PC: optional PCSX2 presets for the pressure-sensitive face buttons, under **Setup → Preset →
  Emulator presets**. `pcsx2-vjoy` (Windows) uses one vJoy device. `pcsx2` uses a new `pressure`
  backend: two Xbox 360 pads per controller, the second carrying A/B/X/Y/White/Black pressure.
  Matching PCSX2 input profiles ship in the release's `pcsx2` folder. See the README's
  "Pressure-sensitive buttons in PCSX2".
- Tested on hardware: none of this yet. The rumble fix is built but untested on a console. The PCSX2
  presets were tested with simulated Xbox reports only, not a physical pad in a PS2 game.

## v0.2.4

- Windows: `xidpipe.exe` now carries version information (name, version, description, licence) in
  Properties > Details, like other Windows programs. Antivirus heuristics tend to distrust executables
  without it; this may help with false positives such as Defender's `Trojan:Win32/Bearfoos.B!m`, but
  that is not confirmed. If Defender flags xidpipe, please report it as a false positive at
  https://www.microsoft.com/en-us/wdsi/filesubmission.
- CI: updated `actions/checkout`, `upload-artifact` and `download-artifact` to their Node 24 versions.

## v0.2.3

Not tested on hardware: CI builds and unit tests only.

- Xbox FTP: paths only reach the dashboard drives (C D E F G X Y Z). `Q:`, xidpipe's own folder under a
  second name, could delete the running XBE or replace it without the XBE check.
- Xbox FTP: a folder that holds the running XBE can't be renamed any more. Renaming it turned off the
  running XBE's delete protection and broke updates.
- Xbox rumble: a rumble write the controller fails is sent again every 250 ms. Before, a lost "motors
  off" could leave the motors running until the next rumble.

## v0.2.2

Tested on real hardware: the self-update from GitHub, FTP self-install, and the network round trip
(back to ~0.12 ms in the Debug tab).

- PC app: the Debug tab's network round trip read ~3 ms since v0.1.1 instead of the real ~0.1 ms. Listing
  the PC's network adapters (for finding the Xbox) happened between taking the timestamp and sending, and
  it also paused input handling for a few milliseconds once a second. Adapters are now only listed while
  searching for the Xbox, and the timestamp is taken right before sending.
- Xbox menu: hold Back for 10 seconds to turn the FTP server on or off, reboot or shut down the Xbox.
  The PC gets a neutral controller while it's open.
- Confirmed on hardware: the update check installed a GitHub release by itself.
- Fixed the Xbox update check failing with "unreadable answer": the Xbox's C library doesn't support
  the `sscanf` format used to read HTTP status lines, so status and version parsing no longer use it.
- Xbox update check: every failure now says exactly what went wrong (name lookup, connection, TLS
  error code, HTTP status, cut-off download) instead of a generic message.
- The Xbox screen shows its DNS server. With a manual IP and no DNS server set in the dashboard, xidpipe
  now uses the router for DNS, so the update check can reach GitHub.
- macOS is no longer supported or built: it can't create virtual controllers, so the PC app could only
  show the controller there.

## v0.2.1

The Xbox app's update check and FTP server are new and not tested on hardware yet. If you're on
v0.1.x, install this version by hand once (see the README); after that the Xbox app updates itself.

- The Xbox app checks GitHub for a newer release when it starts, downloads it, and installs it when you
  hold A for 2 seconds (B skips). HTTPS with BearSSL and built-in root certificates; the download must be
  complete and a valid XBE, and the old XBE is kept until the new one is in place.
- The Xbox app is an FTP server for the whole Xbox, like a dashboard (modelled on xemu-dashboard's):
  user `xbox`, password `xbox`, port 21, passive mode, several connections at once. `/` lists the drives
  that can be read (C, D, E, F, G, X, Y, Z); uploads are written in the background through page-aligned
  buffers; MDTM, REST (resumed downloads) and real file dates are supported. Uploads only replace a file
  once complete, and uploading a new copy of the running XBE installs it and restarts xidpipe.
- D: is now the DVD drive (as on dashboards); xidpipe's own folder is mounted as Q:.
- Either can be turned off with an empty `no-update-check` or `no-ftp` file in xidpipe's folder.
- The Xbox screen shows the version. It's the same as the PC app's, taken from `pc/Cargo.toml`.
- Releases now include `xidpipe-vX.Y.Z-default.xbe`, the file the update check downloads.
- Coming from v0.1.x: install this version by hand once; later updates come by themselves.

## v0.1.1

Tested on real hardware: the Pelican pad from v0.1.0, found automatically on a Windows PC with nine network
adapters, playing through xemu and xbased with no setup. The `zero` preset (no deadzone) worked well there.

- Finding the Xbox works on PCs with several network adapters (VPN, WSL/Hyper-V, VirtualBox, ZeroTier, …):
  the hello also goes to each network's own broadcast address and to the address where the Xbox was last
  found. Before, Windows often sent the broadcast out of the wrong adapter, so the IP had to be typed in.
- The window remembers the preset, Xbox address, polling rate and last Xbox address between runs
  (`settings.toml` next to the presets folder).

## v0.1.0

First release. **xidpipe is vibecoded:** an AI (Claude, through Claude Code) wrote almost all of it.
Expect bugs, and see the README.

Tested on real hardware: one 3rd-party Pelican pad (`ffff:ffff`) on Windows, playing through xemu.
Everything else is built and unit-tested by CI only.

**Downloads**

- `xidpipe-v0.1.0-xbox.zip`: `default.xbe` and `xidpipe.iso` for a softmodded or modchipped Xbox.
- `xidpipe-v0.1.0-windows-x64.zip`: `xidpipe.exe`. Needs the ViGEmBus driver (or vJoy) to create controllers.
- `xidpipe-v0.1.0-linux-x64.tar.gz`: `xidpipe` for x86_64 Linux (built on Ubuntu 24.04, so it needs glibc 2.39 or newer).
- `xidpipe-v0.1.0-macos-arm64.tar.gz`: `xidpipe` for Apple Silicon Macs. View only: macOS can't create virtual controllers.

**What's in it**

- Xbox app: reads any Xbox-protocol (XID) controller, including 3rd-party pads without an XID descriptor,
  and streams the raw reports to the PC over UDP as soon as they arrive.
- Virtual controller on the PC: XInput (Xbox 360) or DualShock 4 through ViGEmBus, or vJoy on Windows;
  Xbox 360 style or generic pad through uinput on Linux.
- Pressure-sensitive buttons on analog outputs (LT/RT, and vJoy's Slider/Dial).
- Rumble back to the controller (Xbox 360 mode on Windows; both modes on Linux), with a strength setting.
- Controller polling: auto (at least 125 Hz) or 125/250/500/1000 Hz.
- Deadzones, button mapping, snap correction for pads that snap to the axes, presets.
- Window with Setup, Button tester (with stick trace) and Debug tabs; command line without a window.
