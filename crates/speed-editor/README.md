# filmcraft-speed-editor

Support for the Blackmagic Design **DaVinci Resolve Speed Editor** (USB `1edb:da0e`) in
FilmCraft / DSPV Cut: its keys run editor commands, the wheel shuttles, jogs and scrolls, and
its LEDs show the jog mode and Snap.

Layer L5. Three parts:

| Module | What | I/O |
|---|---|---|
| `protocol` | key codes, LED bits, report parsing, output reports, the feature-report-6 authentication handshake | none |
| `controller` | the key → command map and the wheel's shuttle / jog / scroll behaviour, as a pure state machine from reports to `Action`s, plus the LED state | none |
| `device` (feature `hid`) | the device thread: finds the device, authenticates, re-authenticates before the device's timeout (it reports ~600 s; we redo it at 80%), reconnects after unplugging, writes LED / jog-mode changes | USB HID via `async-hid` |

`filmcraft-ui-egui` depends on this crate **without** `hid`, so the UI keeps no OS code and
the web build is unaffected. The desktop app (`apps/filmcraft`) enables `hid` on Linux, macOS
and Windows and hands the device thread's `Link` to the UI with `FilmcraftApp::with_speed_editor`.
Each frame the UI runs the controller's actions through `menus::invoke`, the same entry point
as menus, shortcuts and the control channel. `FILMCRAFT_SPEED_EDITOR=0` turns it off.

HID is `async-hid` (MIT): pure Rust over hidraw (Linux), IOKit (macOS) and the Win32 HID API
(Windows); no C library is built or linked. Every thread runs under `catch_unwind`, and device
data is parsed with bounds-checked reads (`protocol` has hostile-input tests).

## Key map

Keys fire on press. A key not listed does nothing yet.

| Key | Command |
|---|---|
| SMART INSRT | `source.insert` (Insert) |
| SRC O/WR | `source.overwrite` (Overwrite) |
| IN / OUT | `markers.markIn` / `markers.markOut`; a second press within 0.4 s clears the point (`markers.clearIn` / `markers.clearOut`), the printed `[CLR]` |
| TRIM IN / TRIM OUT | `trim.ripplePrevious` / `trim.rippleNext` (Q / W) |
| ROLL | Rolling Edit tool |
| SLIP SRC / SLIP DEST | Slip tool / Slide tool |
| CUT | `sequence.removeTransition` (clear the selected transition) |
| DIS | `sequence.applyVideoTransition` (default video transition, Cmd+D) |
| SPLIT | `sequence.addEdit` (Add Edit, Cmd+K) |
| RIPL DEL | `edit.rippleDelete` |
| SNAP | `sequence.snap`; its LED shows the state |
| ESC | `edit.undo` (the printed `[UNDO]`) |
| AUDIO LEVEL | `markers.add` (the printed `[MARK]`) |
| FULL VIEW | `window.maximizeFrame` |
| SOURCE / TIMELINE | focus the Source monitor / the Timeline (so STOP/PLAY and the wheel drive that one) |
| CAM1–CAM9 | `multicam.selectCamera1`–`9` |
| STOP/PLAY | `playback.toggle` |
| SHTL / JOG / SCRL | wheel mode (their LEDs show which) |

Not mapped yet: APPND, RIPL O/WR, CLOSE UP, PLACE ON TOP, TRANS DUR, SMTH CUT, TRANS, SYNC BIN,
VIDEO ONLY, AUDIO ONLY, LIVE O/WR.

## Wheel

- **JOG** (the mode a newly attached device starts in): one frame per 256 wheel units (about
  32 frames per turn). The playhead moves only while the wheel does.
- **SHTL**: turning away from where the mode was entered plays at 0.25×, 0.5×, 1×, 2×, 4× or
  8×, forward or backward, by how far it was turned; back to the middle stops. The stop zone
  is about a tenth of a half turn each way. STOP/PLAY (or stopping playback any other way)
  makes the current wheel position the new middle.
- **SCRL**: one frame per 32 units (about 256 frames per turn).
- With edit points selected (trim mode), JOG and SCRL trim them (`trim.nudge`), like the arrow keys.
- With the Source monitor focused, the wheel drives the Source playhead.

The unit counts are first guesses from the device's ~4096-units-per-half-turn absolute range
and live in `controller::Config`; tune them on hardware.

## Platform notes

- **DaVinci Resolve** takes the device while it runs: quit it first. The status bar says so
  when the device cannot be opened or authenticated.
- **Linux**: the hidraw node is root-only by default. The .deb / .rpm packages install
  `packaging/linux/70-blackmagic-speed-editor.rules` (`TAG+="uaccess"`); for a source build copy
  it to `/etc/udev/rules.d/`, run `sudo udevadm control --reload && sudo udevadm trigger` and
  replug.
- **macOS**: may ask for Input Monitoring permission the first time.
- **Bluetooth**: only USB is handled; the Bluetooth connection is untested.
- **iPadOS**: not supported. iPadOS gives apps no raw access to USB HID devices outside
  DriverKit (an Apple entitlement), so the `hid` feature is not enabled for iOS builds.

## Credit

The protocol (report layouts, key and LED codes, the authentication algorithm) was reverse
engineered by Sylvain Munaut and published as `bmd.py` in
<https://github.com/smunaut/blackmagic-misc> (Copyright (C) 2021 Sylvain Munaut, Apache-2.0).
`protocol.rs` is a port of it; see the workspace `NOTICE`. Blackmagic Design, DaVinci Resolve
and Speed Editor are trademarks of Blackmagic Design; this crate is not affiliated with or
endorsed by them.
