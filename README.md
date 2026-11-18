# xidpipe

> [!WARNING]
> **This project is vibecoded.** An AI (Claude, through Claude Code) wrote almost all of it,
> from one person's instructions and testing on their own hardware. It was reviewed and tested by
> its tools and CI, not by an experienced Xbox homebrew or driver developer.
> Expect bugs and rough edges. Read the code before you trust it with anything that matters. It
> comes with no warranty (see the GPL). Bug reports and fixes are welcome.

Play PC games with an original Xbox controller, including 3rd-party ones, with no USB adapter.
The Xbox does the USB work: a homebrew app on a softmodded/modchipped Xbox reads the controllers
and sends their state over your network to a small app on the PC. That app creates a virtual controller.

```
OG controller --USB--> Xbox (xbox/, nxdk homebrew) --UDP, LAN--> PC (pc/, Rust) --> virtual XInput / DirectInput pad
              <--rumble--                            <--rumble--
```

**Tested on real hardware so far:** one 3rd-party Pelican pad (reports itself as `ffff:ffff`, rejected by
XBlast OS and Gentoox) on Windows, playing through xemu. Everything else is only built and unit-tested by CI.

## Features

- **3rd-party controllers:** any device with an XID interface (class 0x58) works, including pads that don't
  answer the XID descriptor request (nxdk's stock driver drops those). Short reports are zero-padded.
- **Controller API is your choice:** `xinput` (Xbox 360 pad), `dinput` (DualShock 4 on Windows,
  generic pad on Linux) or `vjoy` (Windows DirectInput via vJoy, with two extra axes for pressure buttons).
- **Pressure-sensitive face buttons:** A B X Y Black White can drive analog outputs (e.g. A as an analog accelerator on RT).
- **Rumble:** games' rumble reaches the controller's motors within a few milliseconds, with an adjustable maximum strength.
- **Fast polling:** the Xbox polls controllers at 125 Hz or faster (up to 1000 Hz), whatever the controller asks for.
- **Deadzones:** built-in `xbox` (Xbox defaults) and `zero` (no deadzone) presets, or set your own.
- **Snap correction:** for cheap pads whose firmware snaps the sticks to the axes (see below).
- **Presets:** save, edit and switch presets in the window or as TOML files.
- **Window:** pick and edit presets live, and watch each controller's raw input, its output and the deadzone.
- **Button tester:** a drawn controller that lights up with every button, stick and pressure value, plus a
  stick trace that shows the controller's real gate and any snapping.
- **Debug view:** polling and update rates, gaps between updates (timed on the Xbox and on the PC), lost
  packets, network round trip, raw reports, rumble test buttons and a log.
- Up to 4 controllers (one per Xbox port, kept in port order).

## Latency and reliability

- On the Xbox the USB interrupt wakes the sender directly (USB DPC -> kernel event). There's no frame loop or timer polling.
  The report goes out in one UDP packet straight away.
- On the PC a blocking socket hands the packet to the virtual controller as soon as it arrives.
- Every packet carries the full controller state. The Xbox also resends it every 50 ms, so a lost packet is fixed by the next one.
  The PC drops stale or reordered packets by sequence number.
- If the Xbox or the network goes quiet for 500 ms, the PC releases every input, so nothing stays stuck held down.
- Rumble never waits on a timer. The backend's own thread (ViGEm's notification, or Linux force feedback via
  `poll()`) sends it the moment the game asks. An lwIP callback on the Xbox wakes the main loop, and the motor
  write goes out on the next 1 ms USB frame.
- Rumble is resent every 250 ms. The Xbox stops the motors by itself after 1 s without word from the PC, so a lost
  packet or a crashed PC can't leave them running.
- The window only reads a copy of the live state. The network loop runs on its own thread and never waits for the window.
- Each side finds the other again on its own: the PC says hello every second, and the Xbox follows whichever PC spoke last.
- **Polling:** by default ("auto") the Xbox polls each controller at the rate it asks for, but never slower than
  125 Hz (8 ms). You can raise it to 250, 500 or 1000 Hz in the window (Setup > Controller polling) or with
  `--poll HZ`. Polling faster than a controller asks for is the same trick as USB "overclocking" on PC.
  A controller only produces new data at its own internal rate (many pads: about 125 per second). Faster polling
  can't add updates, but it cuts how long each one waits to be read: up to 8 ms at 125 Hz, 1 ms at 1000 Hz.
- Use **wired Ethernet** for the Xbox and, ideally, for the PC too. Wi-Fi adds jitter. The Debug tab times
  the gap between updates both on the Xbox and on the PC, so you can see whether the controller or the
  network caused a gap.

## Getting it

Every push builds everything on GitHub Actions. Open the latest run under
[Actions](https://github.com/ariiZona/xidpipe/actions) and download the artifacts
(you need to be logged into GitHub):

| artifact | contents |
|---|---|
| `xidpipe-xbox` | `default.xbe` and `xidpipe.iso` for the Xbox |
| `xidpipe-Windows` | `xidpipe.exe` |
| `xidpipe-Linux` | `xidpipe` (Linux x86_64) |
| `xidpipe-macOS` | `xidpipe` (view only, see below) |

Or build it yourself, as below. Either way you also need the runtime pieces listed under
[Running on the PC](#running-on-the-pc).

## Building

Get the code with its submodules (nxdk is pinned as a submodule):

```sh
git clone --recursive https://github.com/ariiZona/xidpipe.git
cd xidpipe
# already cloned without --recursive? run: git submodule update --init --recursive
```

### Xbox app (`xbox/`, needs nxdk's toolchain)

nxdk needs GNU make, CMake, clang, lld, LLVM (for `llvm-lib`), bison, flex and git.
These package lists come from [nxdk's own instructions](https://github.com/XboxDev/nxdk/wiki/Install-the-Prerequisites),
except the ones marked *untested*.

| system | command |
|---|---|
| Debian / Ubuntu / Mint (apt) | `sudo apt install build-essential cmake flex bison clang lld llvm git` |
| Arch / Manjaro / EndeavourOS (pacman) | `sudo pacman -Syu --needed make cmake clang bison flex llvm lld git` |
| Fedora / RHEL / Nobara (dnf, rpm) | `sudo dnf install make cmake clang bison flex lld llvm git` *(untested)* |
| openSUSE (zypper, rpm) | `sudo zypper install make cmake clang bison flex lld git llvm-devel` |
| Alpine (apk) | `sudo apk add build-base cmake clang bison flex lld git llvm` |
| Void (xbps) | `sudo xbps-install -S make cmake clang bison flex lld llvm git` *(untested)* |
| Gentoo (emerge) | `sudo emerge --ask llvm-core/clang llvm-core/lld llvm-core/llvm dev-build/cmake sys-devel/bison sys-devel/flex dev-vcs/git` *(untested)* |
| macOS, Intel or Apple Silicon (Homebrew) | `xcode-select --install`, then `brew install cmake coreutils lld llvm` |
| macOS (MacPorts) | not supported by nxdk (MacPorts has no lld port). Use Homebrew, or build in a Linux VM/container |
| Windows (WSL) | `wsl --install -d Ubuntu`, then use the apt line above inside Ubuntu |
| Windows (MSYS2) | install MSYS2 (`winget install MSYS2.MSYS2`, `choco install msys2` or `scoop install msys2`). In the **MSYS2 MinGW 64-bit** shell, run `pacman -S make cmake git bison flex mingw-w64-x86_64-gcc mingw-w64-x86_64-llvm mingw-w64-x86_64-clang mingw-w64-x86_64-lld` |

On older Ubuntu releases, lld may need to come from [apt.llvm.org](https://apt.llvm.org/).
Avoid clang 19.x up to 20.1.2: nxdk warns those versions can miscompile with optimizations on.

Then build (run the MSYS2 version from the MinGW 64-bit shell):

```sh
cd xbox
nxdk/bin/activate make -j4   # runs make inside nxdk's environment
```

This gives you `xbox/bin/default.xbe` and `xbox/xidpipe.iso`. Copy `default.xbe` to the Xbox (FTP, e.g. to
`E:\Apps\xidpipe\`) and start it from your dashboard. Plug the controller into any port.
The Xbox gets its address by DHCP and shows its IP on screen.

### PC app (`pc/`, Rust 1.88 or newer)

You need Rust (through rustup, so it stays current) and a C toolchain/linker.

**Linux.** Most distro `rustc` packages are too old for this project, so use rustup:

| system | command |
|---|---|
| Debian / Ubuntu / Mint (apt) | `sudo apt install build-essential curl`, then `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Arch / Manjaro (pacman) | `sudo pacman -S --needed base-devel rustup && rustup default stable` |
| Fedora / RHEL (dnf, rpm) | `sudo dnf install gcc rustup && rustup-init` |
| openSUSE (zypper, rpm) | `sudo zypper install gcc rustup && rustup default stable` |
| Alpine (apk) | `sudo apk add build-base rustup && rustup-init` |
| Void (xbps) | `sudo xbps-install -S base-devel rustup && rustup-init` |
| Gentoo (emerge) | `sudo emerge --ask dev-util/rustup && rustup-init` |
| anything else | a C compiler, then the [rustup.rs](https://rustup.rs) script |

**Windows.** The window's Windows dependencies need the MSVC toolchain (Visual Studio Build Tools with the C++ workload):

| package manager | command |
|---|---|
| winget | `winget install Rustlang.Rustup Git.Git` and `winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` |
| Chocolatey | `choco install rustup.install git visualstudio2022buildtools visualstudio2022-workload-vctools` |
| Scoop | `scoop install rustup-msvc git`. Scoop has no Visual Studio Build Tools, so install them with winget or Chocolatey as above |

Using the GNU toolchain (`x86_64-pc-windows-gnu`) instead? Rust's bundled MinGW lacks `dlltool`/`as`, which
the window's dependencies need. Install a full MinGW-w64 first (`scoop install mingw`, or MSYS2's
`mingw-w64-x86_64-toolchain`) and put its `bin` folder on `PATH`.

**macOS** (Intel or Apple Silicon):

| package manager | command |
|---|---|
| Homebrew | `xcode-select --install`, then `brew install rust` |
| MacPorts | `xcode-select --install`, then `sudo port install rust` |

Then build:

```sh
cd pc
cargo build --release      # -> pc/target/release/xidpipe (xidpipe.exe on Windows)
cargo test                 # optional: unit tests
```

## Running on the PC

- **Windows:** install the ViGEmBus driver for `xinput`/`dinput`: `winget install ViGEm.ViGEmBus`,
  `choco install vigembus`, or the [installer](https://github.com/nefarius/ViGEmBus/releases).
  Allow xidpipe through the firewall the first time it runs (private network).
- **vJoy (optional, Windows):** install vJoy. Get the maintained fork from
  [BrunnerInnovation/vJoy](https://github.com/BrunnerInnovation/vJoy/releases) (recommended on Windows 10/11),
  or the original with `winget install ShaulEizikovich.vJoyDeviceDriver`. In "Configure vJoy",
  give device 1 all 8 axes, at least 11 buttons and 1 continuous POV hat (continuous gives diagonals).
  Each pad is one vJoy device: pad 2 uses device 2, and so on. Sticks go on X/Y and Rx/Ry, LT/RT on Z/Rz,
  and `sl0`/`sl1` on Slider/Dial. DirectInput allows 8 axes per device, so at most 4 pressure buttons can
  have their own axis: map them to `lt`, `rt`, `sl0` or `sl1` (e.g. `sl0 = "a"`, `sl1 = "b"`).
- **Linux:** xidpipe needs write access to `/dev/uinput`. Either run it as root or add a udev rule:
  ```sh
  echo 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-xidpipe-uinput.rules
  sudo usermod -aG input $USER    # then log out and back in
  ```
  The window needs a normal desktop session (X11 or Wayland, with OpenGL through Mesa or your GPU driver).
- **macOS:** builds and runs with `backend = "none"` (view only). macOS has no public API for virtual
  game controllers without an Apple-granted driver entitlement.

## Using it

Start `xidpipe` with no arguments to open the window. Every change applies straight away.

- **Setup:** presets (menu, Save, Save as), controller type, vibration strength, pressure, deadzones, snap
  correction, mapping, Xbox address and polling. The right side shows each controller live (grey dot: raw
  stick, coloured dot: what the PC gets, shaded disc: deadzone).
- **Button tester:** the raw controller as the Xbox reads it, plus a stick trace.
- **Debug:** connection and per-pad statistics, rumble tests (adjustable power and length) and the log.

Or run it without a window:

```sh
xidpipe xbox            # preset "xbox"; finds the Xbox by broadcast
xidpipe zero            # zero deadzone
xidpipe xbox --xbox 192.168.1.50   # if broadcast doesn't reach the Xbox (different subnet, several network cards)
xidpipe xbox --poll 1000  # poll the controllers at 1000 Hz
xidpipe xbox --monitor  # also print raw input and mapped output, for tuning deadzones
```

### Sticks that snap to the axes

Some cheap pads zero small stick values in their own firmware. The stick then snaps to straight
up/down/left/right and jumps once you push past the zone. To check:

1. Open the **Button tester**.
2. Rotate each stick all the way round, then sweep it slowly across the axes. The trace shows gaps along
   the axes where the controller snaps, and the tester measures the zone's size.
3. Click **Use … as snap correction**, or set `left_snap_zone` / `right_snap_zone` yourself.

xidpipe then removes the jump: movement out of the zone starts smoothly from zero. The snapping inside the
zone can't be undone, because the controller never sends those values.

## Presets

```sh
xidpipe list                 # built-in and saved presets
xidpipe save racing zero     # copy the "zero" preset to a new "racing" preset, then edit the file it prints
xidpipe racing               # use it
xidpipe ./some/file.toml     # or any preset file by path
```

Saved presets live in `%APPDATA%\xidpipe\presets` (Windows), `~/.config/xidpipe/presets` (Linux) or
`~/Library/Application Support/xidpipe/presets` (macOS). [`pc/presets/xbox.toml`](pc/presets/xbox.toml) documents every setting:

| setting | meaning |
|---|---|
| `backend` | `xinput`, `dinput`, `vjoy` (Windows) or `none` |
| `rumble_strength` | vibration strength 0-100 % (default 100). Scales both motors, so it's also the maximum; 0 turns rumble off |
| `vjoy_device` | vJoy device (1-16) for pad 1 when `backend = "vjoy"`. Pad 2 uses the next one |
| `pressure` | `true`: face buttons send their pressure when mapped to an analog output (`lt`, `rt`, and on vJoy `sl0`, `sl1`). `false`: they're plain on/off buttons |
| `press_threshold` | pressure (0-255) at which an analog button counts as pressed (default 30, from the original Xbox SDK) |
| `analog_deadzone` | deadzone for analog buttons and triggers when they drive an analog output (0-255) |
| `left_stick_deadzone`, `right_stick_deadzone` | radial stick deadzone (0-32000). The `xbox` preset uses XInput's 7849 / 8689 |
| `left_snap_zone`, `right_snap_zone` | size of the controller's own per-axis dead zone (0-32000, 0 = off); removes the jump at its edge |
| `[map]` | `output = "input"`, e.g. `rt = "a"`, `lb = "black"`, `lx = "rx"` to swap sticks, `sl0 = "x"` (vJoy) |

Most games apply their own deadzone on top of xidpipe's. Use `zero` if you want the game's deadzone alone.

Example of an analog accelerator on A, for a racing game:

```toml
backend = "xinput"
pressure = true
[map]
rt = "a"
a = "none"
```

## Protocol

UDP, port 47100 on the Xbox, little-endian. Every packet starts with `"XP"`, version `1` and a type byte.

- PC -> Xbox hello (type `0x10`), 10 bytes, sent every second: header, polling interval in ms (0 = auto,
  else 1/2/4/8), 0, ping u32. The Xbox sends to the last PC that said hello and stops after 3 s of silence.
  It answers each hello right away with a pong (type `3`, 10 bytes, the ping echoed at offset 6), so the PC can
  measure the round trip.
- PC -> Xbox rumble (type `0x11`), 9 bytes: header, pad id, left (heavy) motor u16, right (light) motor u16.
  Sent on change and every 250 ms. The Xbox only accepts it from the PC it streams to.
- Xbox -> PC info (type `4`), 16 bytes, once a second per pad: header, pad id, the controller's own interval (ms),
  the interval used (ms), XID type, XID subtype (0/0 = no XID descriptor), has rumble, controller updates/s u16,
  USB errors u16, longest gap between updates in the last second (u16, units of 0.1 ms; older builds send 14 bytes without it).
- Xbox -> PC state (type `1`) or unplugged (type `2`):

| offset | size | field |
|---|---|---|
| 0 | 2 | `"XP"` |
| 2 | 1 | version (1) |
| 3 | 1 | type |
| 4 | 1 | pad id: 1-4 = front port, 5+ = unknown port |
| 5 | 1 | report length n (0-32) |
| 6 | 2 | USB vendor id |
| 8 | 2 | USB product id |
| 10 | 4 | sequence number (per pad id) |
| 14 | n | raw XID input report |

The Xbox forwards the raw report and all parsing happens on the PC. A quirky controller can then be handled
without rebuilding the Xbox app.

## Rumble support

| backend | rumble |
|---|---|
| `xinput` on Windows (ViGEm) | yes |
| `xinput` / `dinput` on Linux (uinput force feedback) | yes |
| `dinput` on Windows (DS4) | not yet: vigem-client doesn't expose the DS4 rumble request |
| `vjoy` | not yet: vJoy rumble arrives as DirectInput force-feedback effects, which need translating |

## CI

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) builds and tests the PC app on Linux, Windows and macOS
and builds the Xbox `.xbe`/`.iso`, then uploads both as artifacts. It runs on GitHub Actions and on Forgejo/Gitea
Actions. On Forgejo, the Windows and macOS jobs need runners with those labels.

## Not done yet

- USB/IP (making the controller show up on the PC as a real USB OG controller). The point would be xemu, but xemu
  doesn't support USB passthrough yet ("USB passthru is not supported yet, but it will be coming"). Worth
  revisiting once it does.
- Virtual controllers on macOS
- Rumble for `dinput` on Windows and for `vjoy`

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). You can use, change and share xidpipe, but anything you
distribute that's based on it must also be GPL-licensed, with its source available.
nxdk and its components keep their own (GPL-compatible) licenses.
