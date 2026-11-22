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

## Status

Early, and tested on one x17 R2. Readings, mode switching and fan boost work on that unit. Reports
from other Alienware models are welcome as issues: include the probe's output.

## Licence

[MIT](LICENSE).
