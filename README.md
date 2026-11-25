# Open Alienware Command Center

> **Unofficial.** This project is not affiliated with, endorsed by or supported by Dell Technologies.
> Alienware and Alienware Command Center are trademarks of Dell Inc. or its subsidiaries, used here
> only to say what hardware and software this project works with.

An open-source, lightweight replacement for Alienware Command Center's thermal side on the
**Alienware x17 R2**. The app, **Alien Cockpit OSS**, is a small Rust + Tauri program that reads
temperatures and fan speeds and switches the laptop's thermal modes through the same firmware
interface Dell's app uses. It runs without Dell's background services.

## ⚠️ Read before using

- **It changes fan and thermal settings on real hardware.** Wrong settings can make a laptop run
  hot. The firmware's own trip limits still apply, but you use this at your own risk. The software is
  provided as is, with no warranty (see [LICENSE](LICENSE)).
- **Writes are gated to the hardware they were verified on:** Alienware x17 R2 with BIOS 1.32.0. On any
  other model or BIOS the app reads only and refuses to change modes or fans.
- Only one program should drive the fans at a time. Close Alienware Command Center before switching
  modes here.
- It never flashes a BIOS and never changes Windows services. BIOS updates stay with Dell's own tools.
- The firmware only answers administrators, so the app asks for admin once at start (one Windows
  prompt). You can turn that off in Settings; readings then show as unavailable until you restart it as
  administrator.

## What it does

- Live CPU and GPU temperatures, per-zone fan speeds (rpm and percent), with a history chart.
- Live CPU and GPU clocks, like CPU-Z and GPU-Z: every core's running clock (P- and E-cores, turbo included,
  from Windows' own counters) and the NVIDIA GPU's core and memory clocks, power state, power and load (from
  NVIDIA's driver library). Read only while the Cockpit or Performance page is open; `cockpit clocks` on the
  command line.
- Where the power goes: the CPU package (cores, built-in graphics, the rest of the chip) from its own energy
  meters, the NVIDIA GPU against its power limit, and the battery. On battery its discharge is the whole
  laptop's input, against the measured parts and the rest nothing measures; on AC Windows doesn't report the
  adapter's power, so the measured parts are shown.
- Thermal modes: Quiet, Balanced, Performance, Full Speed, Battery Saver and G-Mode.
- Fan boost per fan, through the firmware's own allow-listed calls only.
- An emergency rule: when any sensor stays at the trigger (98 °C by default, can only be lowered) for 5 s,
  every fan goes to full until it has stayed 6 °C cooler for 30 s, then back to your setting.
- Fan fault warnings: a fan standing still while its partner spins, every fan on a hot sensor reading
  0 rpm, and a speed reading that never moves.
- A tray icon, a daily log, and a warning when Windows Energy Saver is limiting performance.
- A read-only check that Dell's services are still set the way you left them.

## Layout

| Path | What |
|---|---|
| `app/` | **Alien Cockpit OSS**: the Tauri app. UI in `app/ui` (React + TypeScript), Rust in `app/src-tauri`. |
| `crates/oem-alienware` | The Alienware (AWCC) WMI call packing, discovery, the method and operation allow-list, decoders and fan rules. Pure code, no Windows calls; tested against a recorded capture. |
| `crates/hw` | Firmware reader and writer over WMI, CPU power, battery and disk health, the service check. |
| `crates/hw-service` | An optional SYSTEM service and its owner-only named pipe. Read-only for now and not installed by the app. |
| `crates/hw-probe` | A read-only probe that prints what the firmware exposes on your unit. |

## Build

Needs Windows 10 or 11, [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org) 20+ and
the WebView2 runtime (already on Windows 11).

```
cargo test --workspace
cd app
npm install
npm run dev                         # UI only, with demo data, at http://localhost:5181
npx tauri build --no-bundle         # release exe in target/release/alien-cockpit.exe
```

To see what your unit exposes without changing anything, run the probe from an elevated terminal:

```
cargo run -p wiregent-hw-probe -- machine
```

## Command line

Everything the app does also runs from a terminal with `cockpit` (crate `crates/cli`), on Windows and
Linux. It uses the same code as the app: the same safety gates for modes and fans, the same settings
file and the same daily log. Add `--json` to any command for one JSON document per line.

```
cargo build -p cockpit-cli --release        # target/release/cockpit(.exe)

cockpit --backend demo status               # the recorded x17 R2 capture, no hardware needed
cockpit info                                # backend, model, BIOS, verified or not, modes, fans
cockpit status --watch 2                    # temperatures, fans by zone, mode, alerts; Ctrl+C stops
cockpit modes
cockpit mode balanced                       # or quiet, performance, full-speed, battery-saver, g-mode, 0xA0
cockpit fans
cockpit fan boost cpu 60                    # a fan id (0x32), cpu, gpu or all; 0 to 100
cockpit fan reset                           # every fan back on the firmware's own curve
cockpit guard --trigger 95                  # the emergency fan rule; fans go back on Ctrl+C or after --count N
cockpit health                              # battery and disk wear, crashes (Windows only)
cockpit drift                               # Dell's services against your rules (Windows only)
cockpit lighting probe                      # read-only, also `lighting show` (Windows only)
cockpit log days                            # also `log read 2026-11-09` and `log cleanup`
cockpit settings get emergencyC
cockpit settings set emergencyC=90          # checked like the app's Settings page
cockpit boost-key target g-mode             # what Fn+F1 would switch to
cockpit --json status
```

`--backend` picks where readings come from: `auto` (the hardware service, else the firmware directly from
an administrator terminal; on Linux the `alienware-wmi` driver), `service`, `direct`, `linux` or `demo`.
`--config-dir DIR` uses another folder for settings and logs. Exit codes: 0 ok, 1 error, 2 refused by a
safety gate or not supported on this system. A running app picks up settings changed here when it
restarts.

## Status

Early, and tested on one x17 R2. Readings, mode switching and fan boost work on that unit. Reports
from other Alienware models are welcome as issues: include the probe's output.

## Licence

[MIT](LICENSE).
