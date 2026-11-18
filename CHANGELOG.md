# Changelog

Each release's notes on GitHub are taken from its section here.

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
