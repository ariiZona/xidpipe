# Changelog

Each release's notes on GitHub are taken from its section here.

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
