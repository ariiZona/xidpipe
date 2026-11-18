# xidpipe

> [!WARNING]
> **This project is vibecoded.** An AI (Claude, through Claude Code) wrote almost all of it,
> from one person's instructions and testing on their own hardware. It was reviewed and tested by
> its tools and CI, not by an experienced Xbox homebrew or driver developer.
> Expect bugs and rough edges. Read the code before you trust it with anything that matters. It
> comes with no warranty (see the GPL). Bug reports and fixes are welcome.

Play PC games with an original Xbox controller, including 3rd-party ones, with no USB adapter.
Your modded Xbox does the USB work: a homebrew app on it reads the controllers and sends their state
over your home network to a small app on the PC. That app creates a virtual controller that games see.

```
OG controller --USB--> Xbox (xbox/, nxdk homebrew) --UDP, LAN--> PC (pc/, Rust) --> virtual XInput / DirectInput pad
              <--rumble--                            <--rumble--
```

**Tested on real hardware so far:** one 3rd-party Pelican pad (reports itself as `ffff:ffff`, rejected by
XBlast OS and Gentoox) on Windows, playing through xemu and xbased. The Xbox app's FTP server and update
check (new in v0.2.0) haven't been tested on hardware yet. Everything else is only built and unit-tested by CI.

## What you need

- An original Xbox that can run homebrew (softmodded or modchipped), with a dashboard that can launch
  `.xbe` files and has an FTP server (UnleashX, XBMC4Xbox, EvoX, …).
- The Xbox and the PC on the same network. **Wired Ethernet is best**; Wi-Fi works but adds delay spikes.
- A PC running Windows or Linux. macOS works only as a viewer (see [Platforms](#platforms)).

## Quick start (no building)

1. Download the latest release from the [Releases page](https://github.com/ariiZona/xidpipe/releases):
   `xidpipe-…-xbox.zip`, plus the one for your PC (`windows-x64` or `linux-x64`).
2. **Xbox:** unzip the Xbox package. Connect to your Xbox with an FTP client (FileZilla, WinSCP, …) and copy
   `default.xbe` into a new folder such as `E:\Apps\xidpipe\`. Start it from your dashboard.
   The screen shows the Xbox's IP address. Plug your controller into any port.
3. **Windows PC:** install the ViGEmBus driver (`winget install ViGEm.ViGEmBus`, or its
   [installer](https://github.com/nefarius/ViGEmBus/releases)). Unzip and run `xidpipe.exe`, and allow it
   through the Windows firewall when asked (private networks).
   **Linux PC:** unpack, give yourself access to `/dev/uinput` (see [Platforms](#platforms)), run `./xidpipe`.
4. The xidpipe window finds the Xbox by itself. Your controller appears and games see an Xbox 360 controller.
   If the Xbox isn't found (for example it's on another subnet), type the IP from the Xbox screen into
   **Xbox address** in the window. The window remembers it, and your other settings, for next time.

From then on the Xbox app keeps itself up to date (see [Updating the Xbox app](#updating-the-xbox-app)).

## Features

- **3rd-party controllers:** any Xbox-protocol (XID, USB class 0x58) device works, including pads that don't
  answer the XID descriptor request (nxdk's stock driver drops those). Short reports are zero-padded.
  The DVD remote and the Steel Battalion controller are ignored.
- **Choose what the PC sees:** `xinput` (Xbox 360 controller), `dinput` (DualShock 4 on Windows, a generic
  non-XInput gamepad on Linux) or `vjoy` (a vJoy DirectInput device on Windows, with two extra axes).
- **Pressure-sensitive buttons:** A B X Y Black White can drive analog outputs, for example A as an analog
  accelerator on RT (see [Presets](#presets)).
- **Rumble:** the game's rumble goes back to the controller's motors. It's sent the moment the game sets it,
  with an adjustable maximum strength. See [Rumble support](#rumble-support) for which modes have it.
- **Polling:** "auto" polls each controller at the rate it asks for, but never slower than 125 Hz.
  You can also pick 125, 250, 500 or 1000 Hz. The Xbox starts in auto (so a pad that asks for 4 ms shows
  "polled every 4 ms" on the Xbox screen) and switches to your choice as soon as the PC connects.
- **Deadzones:** built-in `xbox` (Xbox defaults) and `zero` (no deadzone) presets, or set your own.
- **Snap correction:** for cheap pads whose firmware snaps the sticks to the axes (see below).
- **Button mapping and presets:** remap any button, save and switch presets in the window or as TOML files.
- **Window:** Setup (all settings, applied live, with a live view of each controller), Button tester
  (a drawn controller that lights up with every button, stick and pressure value, plus a stick trace that
  shows the controller's real gate and any snapping) and Debug (rates, gaps, lost packets, network round
  trip, raw reports, rumble tests, log).
- **Command line:** run without a window, with a preset.
- **Self-updating Xbox app:** on start it checks GitHub for a newer release and downloads it. You install
  it with the controller, and xidpipe restarts into it.
- **Built-in FTP server** on the Xbox, for xidpipe's own folder, so you can swap files without going back
  to the dashboard. Uploading a new `default.xbe` installs it and restarts.
- **Several controllers:** each one gets its own virtual controller. Pads on the four front ports are
  numbered 1–4 by port; other Xbox-protocol devices (for example behind an extra hub) get 5 and up.
  The Xbox app handles up to 8.

**Default button mapping** (both built-in presets): A B X Y, Back, Start, d-pad, both sticks and their clicks,
and LT/RT map to the same Xbox 360 controls. **White → LB, Black → RB.** Guide isn't mapped, since the
original pad has no such button.

## Latency and reliability

- On the Xbox the USB interrupt wakes the sender directly (USB DPC -> kernel event). Each new report goes out
  in one UDP packet straight away, without waiting for a timer or frame.
- On the PC a blocking socket hands the packet to the virtual controller as soon as it arrives.
- Every packet carries the full controller state. The Xbox also resends it every 50 ms when nothing changes,
  so a lost packet is fixed by the next one. The PC drops stale or reordered packets by sequence number.
- If the Xbox or the network goes quiet for 500 ms, the PC releases every input, so nothing stays stuck held down.
- Rumble never waits on a timer. The backend's own thread (ViGEm's notification, or Linux force feedback via
  `poll()`) sends it the moment the game asks. An lwIP callback on the Xbox wakes the main loop, and the
  controller's rumble endpoint is polled every 1 ms.
- Once a pad has rumbled, its rumble state is resent every 250 ms. The Xbox stops the motors by itself after
  1 s without rumble from the PC, so a lost packet or a crashed PC can't leave them running.
- The window only reads a copy of the live state. The network loop runs on its own thread and never waits for the window.
- Each side finds the other again on its own: the PC says hello every second, and the Xbox sends to whichever PC spoke last.
  While searching, the PC sends the hello to the address where it last found the Xbox, to `255.255.255.255`,
  and to the broadcast address of each of its networks. The last part matters on PCs with VPN, WSL/Hyper-V or
  VirtualBox adapters, where Windows sends `255.255.255.255` out of only one adapter, often the wrong one.
  Once the Xbox answers, the PC talks to it directly, and it searches again if the Xbox goes quiet for 3 s.
- **Polling:** polling faster than a controller asks for is the same trick as USB "overclocking" on PC.
  A controller only produces new data at its own internal rate (the tested Pelican pad: about 125 per second).
  Faster polling can't add updates, but it cuts how long each one waits to be read: up to 8 ms at 125 Hz,
  1 ms at 1000 Hz. If a pad misbehaves at a high rate, go back to auto.
- The Debug tab times the longest gap between updates both on the Xbox (before the network) and on the PC.
  If only the PC number is high, the network caused it.

## Updating the Xbox app

**Update check.** When it starts, the Xbox app asks GitHub for the latest release (HTTPS, checked against
the root certificates built into it, see [`xbox/roots.h`](xbox/roots.h)). If that release is newer, it
downloads its `default.xbe` next to the running one and asks on screen:

- **hold A for 2 seconds** (on any controller) to install it and restart, or
- **press B** to skip it this time.

The Xbox needs internet access for this; without it, the screen says the check failed and everything else
works as usual. An update is only installed after the download has completed and looks like a valid XBE,
and the old XBE is kept until the new one is in place.

**FTP server.** While it runs, the Xbox app serves its own folder over FTP on port 21: any user name and
password, passive mode (FileZilla's default). It can't reach anything outside that folder. Uploading a new
copy of the running XBE (normally `default.xbe`) checks it, installs it the same safe way, and restarts
xidpipe. Other files are saved as they are. You can't delete or rename the running XBE, only replace it.

**Turning them off.** Create an empty file in xidpipe's folder on the Xbox:

| file name | effect |
|---|---|
| `no-update-check` | no update check (the Xbox never contacts GitHub) |
| `no-ftp` | no FTP server |

Anyone on your network can use the FTP server while xidpipe runs, so use `no-ftp` on networks you don't trust.

**Coming from v0.1.x:** those versions have neither feature, so install v0.2.0 the usual way, with your
dashboard's FTP. After that, updates come by themselves.

The update check reads [GitHub's latest release](https://github.com/ariiZona/xidpipe/releases/latest)
and downloads its `xidpipe-vX.Y.Z-default.xbe` asset. Forks can point it at their own repository:
`make XIDPIPE_REPO=owner/name`.

## Using it

Start `xidpipe` with no arguments to open the window. Every change applies straight away. The window
remembers the preset you used, the Xbox address, the polling rate and where it last found the Xbox, in
`settings.toml` (in the same folder as the saved presets, see [Presets](#presets)).

- **Setup:** presets (menu, Save, Save as), controller type, vibration strength, pressure, deadzones, snap
  correction, button mapping, Xbox address and polling. The right side shows each controller live
  (grey dot: raw stick, coloured dot: what the PC gets, shaded disc: deadzone).
- **Button tester:** the raw controller as the Xbox reads it, before deadzones and mapping, plus a stick trace.
- **Debug:** connection and per-pad statistics, rumble tests (adjustable power and length) and the log.

Or run it without a window:

```sh
xidpipe xbox                       # run with the "xbox" preset; finds the Xbox by broadcast
xidpipe zero                       # run with the "zero" preset (no deadzone)
xidpipe xbox --xbox 192.168.1.50   # talk to this Xbox (when broadcast doesn't reach it: other subnet, several network cards)
xidpipe xbox --poll 1000           # poll the controllers at 1000 Hz
xidpipe xbox --monitor             # also print raw input and mapped output
xidpipe list                       # list presets
xidpipe save racing zero           # copy the "zero" preset to a new "racing" preset to edit
xidpipe --version
```

### Sticks that snap to the axes

Some cheap pads zero small stick values in their own firmware. The stick then snaps to straight
up/down/left/right and jumps once you push past the zone. To check:

1. Open the **Button tester**.
2. Rotate each stick all the way round, then sweep it slowly across the axes. The trace shows gaps along
   the axes where the controller snaps. If an axis never reported a small value (under ~1000), the tester
   shows the zone's size.
3. Click **Use … as snap correction**, or set `left_snap_zone` / `right_snap_zone` yourself.

xidpipe then removes the jump: movement out of the zone starts smoothly from zero. The snapping inside the
zone can't be undone, because the controller never sends those values.

## Platforms

- **Windows:** `xinput` and `dinput` need the ViGEmBus driver: `winget install ViGEm.ViGEmBus`,
  `choco install vigembus`, or the [installer](https://github.com/nefarius/ViGEmBus/releases).
  Allow xidpipe through the firewall the first time it runs (private networks).
- **vJoy (optional, Windows):** install vJoy. Get the maintained fork from
  [BrunnerInnovation/vJoy](https://github.com/BrunnerInnovation/vJoy/releases) (recommended on Windows 10/11),
  or the original with `winget install ShaulEizikovich.vJoyDeviceDriver`. In "Configure vJoy",
  give device 1 all 8 axes, at least 11 buttons and 1 POV hat (a continuous POV allows diagonals).
  Pad 1 uses device 1 (set by `vjoy_device`), pad 2 the next one, and so on. Sticks go on X/Y and Rx/Ry,
  LT/RT on Z/Rz, and `sl0`/`sl1` on Slider/Dial. DirectInput allows 8 axes per device, so at most 4 pressure
  buttons can have their own axis: map them to `lt`, `rt`, `sl0` or `sl1` (e.g. `sl0 = "a"`, `sl1 = "b"`).
- **Linux:** xidpipe needs write access to `/dev/uinput`. Either run it as root or add a udev rule once:
  ```sh
  echo 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-xidpipe-uinput.rules
  sudo usermod -aG input $USER    # then log out and back in
  ```
  `xinput` uses the Xbox 360 controller's USB IDs and the kernel xpad driver's button codes, so SDL, Steam and
  Wine/Proton should treat it as an Xbox 360 controller (not tested on Linux yet). `dinput` is a generic gamepad.
  The window needs a normal desktop session (X11 or Wayland with OpenGL).
- **macOS:** runs with `backend = "none"` only (view and test, no virtual controller). macOS has no public
  API for virtual game controllers without an Apple-granted driver entitlement.
- **Xbox network:** the Xbox app uses the network settings saved on the Xbox (a manual IP set in your
  dashboard) if there are any, and DHCP otherwise.

## Building from source

You only need this to change the code or to use something newer than the latest release.
There are two separate programs: the **Xbox app** (C, built with the nxdk homebrew SDK) and the
**PC app** (Rust). You can build either one on its own.

### 1. Get the code

Install Git (see the tables below) and clone the repository **with its submodules**.
nxdk, the Xbox SDK, comes in as a submodule:

```sh
git clone --recursive https://github.com/ariiZona/xidpipe.git
cd xidpipe
```

Already cloned without `--recursive`? Run `git submodule update --init --recursive` inside it.

### 2. Xbox app

**Tools:** nxdk needs GNU make, CMake, clang, lld, LLVM (for `llvm-lib`), bison, flex and git.
These package lists come from [nxdk's own instructions](https://github.com/XboxDev/nxdk/wiki/Install-the-Prerequisites),
except the ones marked *untested*:

| system | install command |
|---|---|
| Debian / Ubuntu / Mint (apt) | `sudo apt install build-essential cmake flex bison clang lld llvm git` |
| Arch / Manjaro / EndeavourOS (pacman) | `sudo pacman -Syu --needed make cmake clang bison flex llvm lld git` |
| Fedora / RHEL / Nobara (dnf, rpm) | `sudo dnf install make cmake clang bison flex lld llvm git` *(untested)* |
| openSUSE (zypper, rpm) | `sudo zypper install make cmake clang bison flex lld git llvm-devel` |
| Alpine (apk) | `sudo apk add build-base cmake clang bison flex lld git llvm` |
| Void (xbps) | `sudo xbps-install -S make cmake clang bison flex lld llvm git` *(untested)* |
| Gentoo (emerge) | `sudo emerge --ask llvm-core/clang llvm-core/lld llvm-core/llvm dev-build/cmake sys-devel/bison sys-devel/flex dev-vcs/git` *(untested)* |
| macOS, Intel or Apple Silicon (Homebrew) | `xcode-select --install`, then `brew install cmake coreutils lld llvm` |
| macOS (MacPorts) | not supported by nxdk (MacPorts has no lld package). Use Homebrew |
| Windows | use MSYS2, below (or WSL with Ubuntu and the apt line) |

**Windows with MSYS2** (tested on this project's development PC):

1. Install MSYS2: `winget install MSYS2.MSYS2` (or `choco install msys2`, or `scoop install msys2`).
2. From the Start menu, open **MSYS2 MINGW64**. It must be the MINGW64 one, not "MSYS2 MSYS" or "UCRT64".
3. In that window, install the tools:
   ```sh
   pacman -S --needed make cmake git bison flex mingw-w64-x86_64-gcc mingw-w64-x86_64-llvm mingw-w64-x86_64-clang mingw-w64-x86_64-lld
   ```
4. Go to your clone. Windows drives are under `/c/`, `/d/`, … so `C:\src\xidpipe` is `/c/src/xidpipe`:
   ```sh
   cd /c/src/xidpipe
   ```

**Build** (Linux and macOS terminal, or the MSYS2 MINGW64 window):

```sh
cd xbox
nxdk/bin/activate make -j4
```

The Xbox app also uses [BearSSL](https://www.bearssl.org/) (TLS for the update check), which comes in as
another submodule, so there's nothing extra to install. Its version number comes from `pc/Cargo.toml`.
To run its unit tests on the PC (any C compiler): `cc -std=c11 -o util_test tests/util_test.c util.c && ./util_test`.

`nxdk/bin/activate` sets up nxdk's environment, then runs `make` in it. The first build also compiles nxdk's
libraries and tools, so it takes a few minutes; later builds are quick. You get `xbox/bin/default.xbe` (copy
this to the Xbox, as in the [Quick start](#quick-start-no-building)) and `xbox/xidpipe.iso`.
Avoid clang 19.x up to 20.1.2: nxdk warns that these versions can miscompile.

### 3. PC app

**Tools:** Rust 1.88 or newer (installed with rustup, so it stays current) and a C compiler/linker.

**Linux.** Most distros package an older Rust, so use rustup:

| system | install command |
|---|---|
| Debian / Ubuntu / Mint (apt) | `sudo apt install build-essential curl git`, then `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Arch / Manjaro (pacman) | `sudo pacman -S --needed base-devel rustup git && rustup default stable` |
| Fedora / RHEL (dnf, rpm) | `sudo dnf install gcc rustup git && rustup-init` |
| openSUSE (zypper, rpm) | `sudo zypper install gcc rustup git && rustup default stable` |
| Alpine (apk) | `sudo apk add build-base rustup git && rustup-init` |
| Void (xbps) | `sudo xbps-install -S base-devel rustup git && rustup-init` |
| Gentoo (emerge) | `sudo emerge --ask dev-util/rustup dev-vcs/git && rustup-init` |
| anything else | a C compiler, then the [rustup.rs](https://rustup.rs) script |

**Windows.** You need Rust, Git and Microsoft's C++ build tools (Visual Studio Build Tools). The window's
Windows dependencies need the MSVC toolchain.

| package manager | install commands |
|---|---|
| winget | `winget install Rustlang.Rustup Git.Git`, then `winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` |
| Chocolatey | `choco install rustup.install git visualstudio2022buildtools visualstudio2022-workload-vctools` |
| Scoop | `scoop install rustup-msvc git`. Scoop has no Build Tools, so install those with winget or Chocolatey as above |

Afterwards, open a **new** terminal so the tools are on `PATH`. If you already had Rust set up for the GNU
toolchain, switch this project to MSVC: run `rustup override set stable-x86_64-pc-windows-msvc` inside `pc`.
(Want to stay on GNU? Rust's bundled MinGW lacks `dlltool`/`as`, which the window's dependencies need.
Install a full MinGW-w64, e.g. `scoop install mingw` or MSYS2's `mingw-w64-x86_64-toolchain`, and put its
`bin` folder on `PATH`.)

**macOS** (Intel or Apple Silicon):

| package manager | install commands |
|---|---|
| Homebrew | `xcode-select --install`, then `brew install rust` |
| MacPorts | `xcode-select --install`, then `sudo port install rust` |

**Build:**

```sh
cd pc
cargo build --release      # the program: pc/target/release/xidpipe (xidpipe.exe on Windows)
cargo test                 # optional: run the unit tests
```

The first build downloads and compiles the Rust dependencies, which takes a few minutes.

## Presets

Saved presets live in `%APPDATA%\xidpipe\presets` (Windows), `~/.config/xidpipe/presets` (Linux) or
`~/Library/Application Support/xidpipe/presets` (macOS). The window's Save / Save as write them there, and so
does `xidpipe save`. The built-in `xbox` and `zero` can't be overwritten.
[`pc/presets/xbox.toml`](pc/presets/xbox.toml) documents every setting:

| setting | meaning |
|---|---|
| `backend` | `xinput`, `dinput`, `vjoy` (Windows) or `none` |
| `rumble_strength` | vibration strength 0-100 % (default 100). Scales both motors, so it's also the maximum; 0 turns rumble off |
| `vjoy_device` | vJoy device (1-16) for pad 1 when `backend = "vjoy"`. Pad 2 uses the next one |
| `pressure` | `true`: A B X Y Black White send their pressure when mapped to an analog output (`lt`, `rt`, `sl0`, `sl1`). `false`: they're plain on/off buttons |
| `press_threshold` | pressure (0-255) at which an analog button counts as pressed (default 30, from the original Xbox SDK) |
| `analog_deadzone` | deadzone for the 8 analog buttons (incl. LT/RT) when they drive an analog output (0-255) |
| `left_stick_deadzone`, `right_stick_deadzone` | radial stick deadzone (0-32000). The `xbox` preset uses XInput's 7849 / 8689 |
| `left_snap_zone`, `right_snap_zone` | size of the controller's own per-axis dead zone (0-32000, 0 = off); removes the jump at its edge |
| `[map]` | `output = "input"`, e.g. `rt = "a"`, `lb = "black"`, `lx = "rx"` to swap sticks, `sl0 = "x"`. Outputs not listed keep the default mapping. `sl0`/`sl1` only reach games with vJoy |

Most games apply their own deadzone on top of xidpipe's. Use `zero` if you want the game's deadzone alone.

Example of an analog accelerator on A, for a racing game:

```toml
backend = "xinput"
pressure = true
[map]
rt = "a"
a = "none"
```

## Rumble support

| backend | rumble |
|---|---|
| `xinput` on Windows (ViGEm) | yes |
| `xinput` / `dinput` on Linux (uinput force feedback) | yes |
| `dinput` on Windows (DS4) | not yet: vigem-client doesn't expose the DS4 rumble request |
| `vjoy` | not yet: vJoy rumble arrives as DirectInput force-feedback effects, which need translating |

The controller also needs rumble motors and an OUT endpoint; the Debug tab shows whether it has one.

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
| 4 | 1 | pad id: 1-4 = front port, 5+ = other |
| 5 | 1 | report length n (0-32) |
| 6 | 2 | USB vendor id |
| 8 | 2 | USB product id |
| 10 | 4 | sequence number (per pad id) |
| 14 | n | raw XID input report |

The Xbox forwards the raw report and all parsing happens on the PC. A quirky controller can then be handled
without rebuilding the Xbox app.

## CI and releases

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) builds and tests the PC app on Linux, Windows and macOS
and builds the Xbox `.xbe`/`.iso` on every push, then uploads the builds as artifacts. Pushing a tag like
`v0.1.0` (it must match the version in `pc/Cargo.toml`) also publishes a GitHub release with those builds
attached, and its notes come from that version's section in [CHANGELOG.md](CHANGELOG.md). The release
includes `xidpipe-vX.Y.Z-default.xbe` on its own: that's the file installed Xbox apps download to update.
The workflow also runs on Forgejo/Gitea Actions (minus the release step). There, the Windows and macOS jobs
need runners with those labels.

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
