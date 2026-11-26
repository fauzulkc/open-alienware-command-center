# Performance

The owner's rule (2026-11-25): performance first, then features. A richer app must not cost more than it shows.
Every change that touches the window, a poll or a background loop is measured with
`scripts/perf-snapshot.ps1` before and after, in the states below, and the numbers go in this file.

## Budget

| State | RAM (working set, all processes) | CPU (% of one core) | GPU | Notes |
|---|---|---|---|---|
| Tray only (window closed) | ≤ 60 MB | ≤ 1 % idle, ≤ 10 % with live lighting | 0 | The web view is freed on close. |
| Window open, behind other windows | ≤ 300 MB | ≤ 10 % | ≤ 1 % | Nothing seen; nothing should repaint. |
| Window open, Cockpit in front | ≤ 300 MB | ≤ 15 % | ≤ 3 % | Rotors spinning, charts updating each second. |
| Page switch | — | — | — | Under 100 ms to the new page drawn. |
| Disk | app ≤ 20 MB; logs within their cap | | | |

## Baseline, 2026-11-25 (before the first fixes)

The installed build. Live lighting on (screen mirror, seismic taps, mouse boost); the window open on the Cockpit, behind
other windows (it runs as administrator, so a normal tool can't minimise or hide it to measure those states).

| Process | Working set | Committed | CPU, % of a core | GPU |
|---|---|---|---|---|
| WebView2 GPU process | 106 MB | 485 MB | 52-60 % | 11.4-11.8 % (Intel 3D) |
| WebView2 renderer | 143 MB | 90 MB | 43-50 % | 0 |
| WebView2 browser + utilities + crashpad | 196 MB | 55 MB | 1-3 % | 0 |
| alien-cockpit (Rust: polls, live lighting) | 109 MB | 139 MB | 17-30 % | 0.15 % |
| **Total, 7 processes** | **~554 MB** | **~770 MB** | **~120-140 %** | **~11.7 %** |

Disk: `alien-cockpit.exe` 13.5 MB, `cockpit.exe` 4.3 MB, the web view's profile 60 MB, logs 21 MB in 13 days (cap 200 MB).

What it showed: the web view repainted all the time, unseen. The four fan rotors redraw on every display refresh
(the x17's 4K panel runs at 120 Hz), and every panel had a backdrop blur, so each rotor frame made the GPU blur
4K-sized panels again: about one and a half cores and the Intel GPU's 3D engine busy, for a window nobody was looking
at. The blur showed nothing, because the panels sit on the flat canvas colour.

## Fixes

1. Panels lose `backdrop-filter` (no visible change; floating read-outs keep theirs, they're small and brief).
2. The rotors share one animation loop capped at 60 frames a second, which stops when no rotor is on screen.

After (2026-11-25, 20 s, the Cockpit **in front**, live lighting on, the laptop itself busy at 100 °C):

| Process | Working set | Committed | CPU, % of a core | GPU |
|---|---|---|---|---|
| WebView2 renderer | 173 MB | 118 MB | 20 % | 0 |
| WebView2 GPU process | 96 MB | 210 MB | 21 % | 3.7 % |
| WebView2 browser + utilities + crashpad | 191 MB | 55 MB | 1 % | 0 |
| alien-cockpit | 99 MB | 130 MB | 23 % | 0.2 % |
| **Total** | **~560 MB** | **~513 MB** | **~65 %** | **~3.9 %** |

The web view's CPU fell from about a core to 0.4 of one and its GPU use to a third, in a heavier state (in front, not
covered). Committed memory fell by about 260 MB (the GPU process no longer holds blur surfaces). Still over budget:
the next candidates below.

### Round 2 (2026-11-25)

3. The rotor blades are layers of their own, turned with a CSS transform: a frame is composited, not repainted.
4. Minimised, the web view is told it's invisible (WebView2 `SetIsVisible(false)`) with a low memory target; wry
   only skipped the resize. The clocks read (PDH, NVIDIA) skips while the page is hidden.
5. Bars that change every second (processes, clocks, power) grow by `transform: scaleX` instead of `width`, so their
   easing runs on the compositor.

An A/B build with the per-second transitions off showed what they cost: GPU 4.8 % → 0.7 %, the web view's CPU about
37 % → 24 % of a core. Moving the bars to the compositor kept their easing for most of that saving; the gauge and
rotor arcs still ease (they change only when a reading does) and account for the rest.

Cockpit in front, 20 s, live lighting on, the laptop under load (G-Mode, CPU at 100 °C):

| | Baseline (covered) | Round 1 | Round 2 |
|---|---|---|---|
| Web view CPU (renderer + GPU process) | ~100 % of a core | ~41 % | ~31 % |
| alien-cockpit CPU | 17-30 % | 23 % | 22 % |
| GPU | 11.7 % | 3.9 % | 1.9 % |
| Committed memory | ~770 MB | ~513 MB | ~489 MB |
| Working set | ~554 MB | ~560 MB | ~538 MB |

GPU is within budget. CPU (~54 % of a core) and memory are not yet. Minimised and tray states still need measuring
with the window driven by hand (the app runs as administrator, so a normal tool can't minimise it).

### The redesign build (2026-11-26)

The new Cockpit puts the rotors on the first screen, so they always spin when the Cockpit is open, and every frame
makes the web view compose the whole 4K window again. Measured, Cockpit in front, 20 s:

| | Round 2 (rotors below the fold) | Redesign, rotors at 60 fps | Redesign, rotors at 30 fps |
|---|---|---|---|
| Web view CPU | ~31 % of a core | ~48 % | ~29 % |
| GPU | 1.9 % | 5.3 % | 3.5 % (web view 2.3, the screen mirror 1.2) |
| Committed memory | ~489 MB | ~512 MB | ~504 MB |

6. The rotors draw at 30 frames a second. Each frame still advances at most 1/48 s, under half a blade gap, so they
   can't strobe; at top speed they turn about 1.25 times a second instead of 2.

### Compositor rotors (2026-11-26)

The owner found the 30-frame rotors "clanky in high speed": a blade jumped 15° a frame and the trailing ghost copy
doubled the image.

7. The blades turn in a Web Animations loop on the compositor, at the display's own refresh (120 Hz), with no
   per-frame script; inertia changes only the playback rate, ten times a second. A rotor that is stopped or off
   screen is paused. The ghost copy is gone: at speed a still blur disc fades in.

Cockpit in front, 20 s, live lighting on (screen mirror and mouse boost):

| | Redesign, 30-frame script | Compositor rotors |
|---|---|---|
| Renderer CPU | ~18 % of a core | ~8 % |
| GPU process CPU | ~11 % | ~21 % |
| Web view CPU | ~29 % | ~29 % |
| GPU | 3.5 % (web view 2.3) | 1.4 % (web view 1.2) |
| Committed memory | ~504 MB | ~419 MB |

Four times the frames for the same CPU: the work moved from the page's thread to the compositor, and the GPU
does less without the ghost layer.

## Next candidates, in order of expected gain

- The Rust process: ~22 % of a core and ~100 MB. Split it: live lighting's loop (wakes every 20 ms whatever runs),
  the screen grab, the 1-second IPC answers, the clocks read. Measure each with a build that turns one off.
- The 1-second poll re-renders the whole page; give each panel its own subscription so only what changed draws.
- The `clocks` read (PDH and NVIDIA's driver) every second while the Cockpit is open: every 2 s, or only while
  the gauges are on screen.
- Memory: the web view's ~430 MB working set across six processes is most of the total; a covered (not minimised)
  window could get the same low memory target after a minute unfocused.

## How to measure

```powershell
powershell -File scripts\perf-snapshot.ps1 -Seconds 15 -Label 'Cockpit in front'
```

Measure: the window in front on the Cockpit; behind other windows; minimised; closed to the tray; and each with
live lighting on and off. Compare like with like (same page, same lighting, same power source).
