# xidpipe

Play PC games with an original Xbox controller, including 3rd-party ones, with no USB adapter.
The Xbox does the USB work: a homebrew app on a softmodded/modchipped Xbox reads the controllers
and sends their state over your network to a small app on the PC. That app creates a virtual controller.

```
OG controller --USB--> Xbox (xbox/, nxdk homebrew) --UDP, LAN--> PC (pc/, Rust) --> virtual XInput / DirectInput pad
              <--rumble--                            <--rumble--
```

- **3rd-party controllers:** any device with an XID interface (class 0x58) works, including pads that don't
  answer the XID descriptor request (nxdk's stock driver drops those). Short reports are zero-padded.
- **Controller API is your choice:** `xinput` (Xbox 360 pad), `dinput` (DualShock 4 on Windows,
  generic pad on Linux) or `vjoy` (Windows DirectInput via vJoy, with two extra axes for pressure buttons).
- **Pressure-sensitive face buttons:** A B X Y Black White can drive analog outputs (e.g. A as an analog accelerator on RT).
- **Rumble:** games' rumble goes back to the controller's motors.
- **Deadzones:** built-in `xbox` (Xbox defaults) and `zero` (no deadzone) presets, or set your own.
- **Presets:** save, edit and switch presets in the window or as TOML files.
- **Window:** pick and edit presets live, and watch each controller's raw input, its output and the deadzone.
- Up to 4 controllers (one per Xbox port, kept in port order).

## Latency and reliability

- On the Xbox the USB interrupt wakes the sender directly (USB DPC -> kernel event). There's no frame loop or timer polling.
  The report goes out in one UDP packet straight away.
- On the PC a blocking socket hands the packet to the virtual controller as soon as it arrives.
- Every packet carries the full controller state. The Xbox also resends it every 50 ms, so a lost packet is fixed by the next one.
  The PC drops stale or reordered packets by sequence number.
- If the Xbox or the network goes quiet for 500 ms, the PC releases every input, so nothing stays stuck held down.
- Rumble is resent every 250 ms. The Xbox stops the motors by itself after 1 s without word from the PC, so a lost
  packet or a crashed PC can't leave them running.
- The window only reads a copy of the live state. The network loop runs on its own thread and never waits for the window.
- Each side finds the other again on its own: the PC says hello every second, and the Xbox follows whichever PC spoke last.
- The controller's own USB poll rate (usually 4-8 ms) is the biggest delay. Use **wired Ethernet** for the Xbox and,
  ideally, for the PC too. Wi-Fi adds jitter.

## Setup

### 1. Xbox app

You need [nxdk](https://github.com/XboxDev/nxdk) and its prerequisites (GNU make, clang, lld, bison, flex, cmake, git).
It builds on Linux, macOS and Windows (via WSL). nxdk is a pinned git submodule:

```sh
git submodule update --init --recursive
cd xbox
nxdk/bin/activate make -j4   # runs make inside nxdk's environment
```

This gives you `xbox/bin/default.xbe` (and `xbox/xidpipe.iso`). Copy `default.xbe` to the Xbox (FTP, e.g. to `E:\Apps\xidpipe\`)
and start it from your dashboard. Plug the controller into any port.
The Xbox gets its address by DHCP and shows its IP on screen.

### 2. PC app

Build with Rust 1.88+ (`pc/`), or download a build from CI:

```sh
cd pc
cargo build --release      # -> pc/target/release/xidpipe(.exe)
```

- **Windows:** install [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) for `xinput`/`dinput`.
  Allow xidpipe through the firewall the first time it runs (private network).
  Build with the MSVC Rust toolchain (needs Visual Studio Build Tools). The GNU toolchain needs a full MinGW-w64
  install (`dlltool` and `as`), because the window's Windows dependencies build import libraries.
- **vJoy (optional, Windows):** install [vJoy](https://github.com/BrunnerInnovation/vJoy/releases). In "Configure vJoy",
  give device 1 all 8 axes, at least 11 buttons and 1 continuous POV hat (continuous gives diagonals).
  Each pad is one vJoy device: pad 2 uses device 2, and so on. Sticks go on X/Y and Rx/Ry, LT/RT on Z/Rz,
  and `sl0`/`sl1` on Slider/Dial. DirectInput allows 8 axes per device, so at most 4 pressure buttons can
  have their own axis: map them to `lt`, `rt`, `sl0` or `sl1` (e.g. `sl0 = "a"`, `sl1 = "b"`).
- **Linux:** xidpipe needs write access to `/dev/uinput`. Either run it as root or add a udev rule:
  ```sh
  echo 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-xidpipe-uinput.rules
  sudo usermod -aG input $USER    # then log out and back in
  ```
- **macOS:** builds and runs with `backend = "none"` (monitor only). macOS has no public API for virtual
  game controllers without an Apple-granted driver entitlement.

### 3. Play

Start `xidpipe` with no arguments to open the window. Every change applies straight away. The preset menu,
Save and Save as manage presets, and the right side shows each controller live (grey dot: raw stick,
coloured dot: what the PC gets, shaded disc: deadzone).

Or run it without a window:

```sh
xidpipe xbox            # preset "xbox"; finds the Xbox by broadcast
xidpipe zero            # zero deadzone
xidpipe xbox --xbox 192.168.1.50   # if broadcast doesn't reach the Xbox (different subnet, several network cards)
xidpipe xbox --monitor  # also print raw input and mapped output, for tuning deadzones
```

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
| `vjoy_device` | vJoy device (1-16) for pad 1 when `backend = "vjoy"`. Pad 2 uses the next one |
| `pressure` | `true`: face buttons send their pressure when mapped to an analog output (`lt`, `rt`, and on vJoy `sl0`, `sl1`). `false`: they're plain on/off buttons |
| `press_threshold` | pressure (0-255) at which an analog button counts as pressed (default 30, from the original Xbox SDK) |
| `analog_deadzone` | deadzone for analog buttons and triggers when they drive an analog output (0-255) |
| `left_stick_deadzone`, `right_stick_deadzone` | radial stick deadzone (0-32000). The `xbox` preset uses XInput's 7849 / 8689 |
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

- PC -> Xbox hello (type `0x10`), 4 bytes, sent every second. The Xbox sends to the last PC that said hello and stops after 3 s of silence.
- PC -> Xbox rumble (type `0x11`), 9 bytes: header, pad id, left (heavy) motor u16, right (light) motor u16.
  Sent on change and every 250 ms. The Xbox only accepts it from the PC it streams to.
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

- Virtual controllers on macOS
- Rumble for `dinput` on Windows and for `vjoy`

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). You can use, change and share xidpipe, but anything you
distribute that's based on it must also be GPL-licensed, with its source available.
nxdk and its components keep their own (GPL-compatible) licenses.
