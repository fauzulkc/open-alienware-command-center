# Lighting and macros: parity with Alienware Command Center

Plan, 2026-11-22. Facts come from this laptop (read-only checks of the devices and of AWCC 6.13.8.0's own
files) and from public sources, cited at the end. Nothing here has been written to a device yet.

## What the hardware is

| Device | Id | Protocol | Lights |
|---|---|---|---|
| Zone controller ("AW-ELC") | USB `187C:0550`, vendor page `FF00:0001` | AlienFX API v4: 34-byte output reports | 12: power button (id 0, can't be turned off, shows power status), alien head on the lid (id 1), rear loop ("stadium"), left 0–4 (ids 10–14) and right 0–4 (ids 15–19) |
| Keyboard: x17 R2 Cherry mechanical, per-key RGB | USB `0D62:CABC` (Darfon), vendor collection `FF89:00CC` | AlienFX API v5: 64-byte feature report 0xCC | 91 keys, AWCC ids 0–135 (sparse); AWCC groups: Function keys (12), Numbers (10), QWER (4), WASD (4) |

Neither device is a Windows Dynamic Lighting (LampArray) device, so Windows can't drive them; the only
LampArray here is the Logitech mouse. The keyboard has no lighting slots for power states in its
description, so after AWCC is gone, nothing turns it off for sleep or battery unless this app does.

## What AWCC offers on this laptop, and where the app stands

| AWCC feature | On this laptop | App today | Priority |
|---|---|---|---|
| Lighting on/off, brightness (Go Light / Go Dim / Go Dark) | All devices | Shows nothing | **L1** |
| Keyboard and zones off or dim in sleep, on battery, lid closed | Zones: 6 firmware power-state slots (AC sleep, AC charged, AC charging, battery sleep, battery on, battery low). Keyboard: none, AWCC does it in software | Nothing | **L1** (a lit keyboard in sleep drains the battery) |
| One colour per zone and for the whole keyboard | Yes | Read-only view of AWCC's preset | **L1** |
| Hardware effects | Keyboard: colour, breathing, spectrum, rainbow wave, scanner. Zones: colour, pulse, morph, breathing, spectrum, rainbow wave. Settings: colour palette (12), brightness, tempo, duration, direction | Effect buttons, all disabled | **L1** |
| Import the current AWCC look | Active preset "System Default": keyboard 11 colour groups over 91 keys; zones run the power animations and Spectrum | Read (best effort: AWCC's key ids aren't device indices yet) | **L2** |
| Per-key colours, key groups | Yes | No | **L3** |
| Lighting per game | Yes, but off here (69 presets created by AWCC, all the same) | Game list read for modes only | **L3** |
| Key binds on F2–F6: remap, keystroke, recorded macro, shortcut (launch a program), text block; repeat while held or toggled; 0–1000 ms delays | Yes (AWCC's description of this keyboard: 5 macro keys, 1 mode) | No | **L4**: you have none set |
| Store presets to device memory, P / Fn+F3 slot switching | External Alienware keyboards only | — | Not needed |
| Stealth mode (Fn+F2) | Not on the x17 R2 | — | Not needed |
| Lighting firmware updates | Dell's tool | — | Never in this app |
| Windows Dynamic Lighting | Not supported by these devices | — | Not planned (needs a driver) |

Two things beyond AWCC that are worth building once L1 works, because they carry information:
- **Status lighting:** the alien head (or the loop) turns red while the emergency fan rule has the fans, and
  amber while every CPU fan reads 0 RPM on a hot CPU. Off by default.
- **Temperature colour:** the loop shows CPU temperature on a blue-to-red scale. Off by default.

Audio-reactive and game-reactive effects are extras; they come last, if at all.

## Two rules from the owner (2026-11-22)

1. **The existing lighting configuration must not be lost.** Before any write, back up AWCC's
   `FXRepository.db` and `FXMetadata` folder to a dated folder and read the zone controller's power-state
   slots. The app never writes AWCC's database. Every test ends with AWCC's own look back and checked.
   Writes to the controller's memory (the power-state slots) come only after their current contents are
   saved and can be written back.
2. **Right Ctrl is the Tab key.** The physical Tab key is broken; PowerToys Keyboard Manager maps Right Ctrl
   to Tab. Nothing in this app may take, block or re-inject Right Ctrl or Tab: the boost-key listener only
   reads, and key binds (L4) pass injected keys through untouched and refuse Right Ctrl and Tab as bind
   keys. PowerToys runs without admin rights, so its remap doesn't reach windows that run as administrator,
   Alien Cockpit included (see below).

## Order of work

### L0: prove the protocols (one session with you, reversible)
0. Back up AWCC's lighting files (rule 1) and read both devices' current state before anything else.
1. Port the v4 and v5 packet builders from alien-tools (MIT) into a new pure crate, `crates/alienfx`, with
   byte-exact tests against alien-tools' own packets. No device access in this crate, like `oem-alienware`.
2. A read-only probe: open both devices, read their status and firmware version, list collections.
3. **One write test, with you there:** AWCC's lighting services (`AlienFXSubAgent`, `FxDisplayService`)
   paused for the session (one writer only: two writers can freeze the controller), then
   - the alien head to one colour, then back to AWCC's colour;
   - one key on the keyboard to one colour, then back;
   - check that the light changes and that AWCC restores everything when its services start again.
   Recovery if a controller freezes: unplug the charger, hold power off, wait about 15 s.
4. Map AWCC's key ids (0–135) to the keyboard's indices from this test, and record it as a fixture.

Gate: both writes seen, both restored, the id map recorded.

### L1: everyday lighting (most of what people use)
- Brightness and lights off for the keyboard and the zones, from the app and the tray.
- One colour per zone and for the whole keyboard; the hardware effects with their settings.
- **Power behaviour without AWCC:** zones: write the 6 power-state slots once (these go to the controller's
  memory, so they are a separate step with its own yes). Keyboard: the app turns it off or dims it on
  sleep, lid close, display off and battery saver, and restores it on wake (Windows power events).
- Lighting writes go through the same safety pattern as fans: an allow-list of packets, one writer (refuse
  while AWCC's lighting services run), rate limit, read-back where the device allows it.
- Status lighting (above), off by default.

Gate: a day of normal use with AWCC's lighting services stopped: lights follow sleep, lid, battery and
wake; nothing stays lit in sleep; AWCC restores its own look when started again.

### L2: import the AWCC look
- Read `FXRepository.db` (already done read-only) and AWCC's device descriptions in
  `C:\ProgramData\Alienware\Alienware Command Center\FXMetadata\` (signed, base64 JSON; the keyboard's has
  trailing commas, so parse leniently). Apply the active preset with the L0 id map.

Gate: the imported look matches AWCC's on the laptop, checked side by side.

### L3: per-key editor, groups, per-game lighting
- A keyboard map of the x17 R2 layout with AWCC's four groups; click or drag to paint keys.
- Software effects for per-key animation (v5 has no per-key hardware effects; software and hardware effects
  can't run together).
- Per-game lighting, using the game list the app already reads.

### L4: key binds on F2–F6
- Types: remap, keystroke, recorded macro (with delays), shortcut (start a program), text block; repeat
  once, while held, or toggled.
- Built with a low-level keyboard hook and `SendInput`. Two cautions: some anti-cheat systems flag injected
  input, so binds pause in a list of games (on by default for known anti-cheat titles); and a hook in an
  elevated app can type into elevated windows, so binds only run what you recorded.
- Off until you add a bind. You have none in AWCC today, so there is nothing to migrate.
- Right Ctrl and Tab can't be bound, and injected keys (PowerToys' Tab) pass through untouched.

### Key remapping, as part of the keyboard map (owner's call, 2026-11-22: later, with L3)
Remapping belongs to the keyboard map: the same picture of the x17 R2 keyboard used for per-key lighting
also shows and edits what each key does. Until then the Right Ctrl → Tab remap stays in PowerToys as it is.

Why it's not a plain copy of PowerToys: PowerToys' remap is a hook in a normal-rights program, so Windows
keeps it out of windows that run as administrator (in those, Right Ctrl stays Right Ctrl). A one-to-one key
remap in the keyboard map is written as Windows' own key map (registry `Scancode Map`), which works
everywhere, including admin windows and the sign-in screen, with no program running: admin once, then a
sign-out; undone by removing the entry. Binds that do more than swap one key (shortcuts, text, macros) stay
in L4. Moving Right Ctrl → Tab over from PowerToys happens with the owner, and PowerToys' entry is removed
only after the new one is checked, so Tab never stops working.

## How it's tested

| Level | What |
|---|---|
| Unit | Packet builders byte-exact against alien-tools; allow-list exhaustive; id map round trip; power-event state machine (sleep, wake, lid, battery) |
| Fake device | A recorded-replay HID device, like the firmware replay: every UI action produces exactly the expected packets and nothing else; one-writer refusal while AWCC's services run |
| On the laptop | L0's write test; L1's day of use; L2's side-by-side check; all with AWCC restored afterwards |

## Open questions

1. Is per-key editing (L3) worth it to you, or are whole-keyboard colours and the zones enough?
2. Do you want key binds on F2–F6 (L4) at all?
3. What does Fn+F7 do on your laptop now: cycle the keyboard brightness, or turn it off? And does it still
   work with AWCC closed? (Dell's manual says it's keyboard brightness; whether the keyboard or AWCC
   handles it decides whether the app must.)
4. L0 needs AWCC's lighting services paused for one session. OK to do that with you present?
5. ~~Fix the Right Ctrl → Tab remap for admin windows~~: decided, later, as part of the keyboard map (L3).

## Sources

- This laptop: device listing (`187C:0550`, `0D62:CABC` collections), AWCC's `FXRepository.db` and
  `FXMetadata` device descriptions, read-only, 2026-11-22.
- Dell, Alienware x17 R2 setup and specifications: AlienFX zones, keyboard, keyboard shortcuts.
  https://www.dell.com/support/manuals/en-us/alienware-x17-r2-laptop/alienware-x17-r2-setup-and-specifications/alienware-command-center?guid=guid-ad56ecd6-2771-4f6c-b7bd-628c1959636c&lang=en-us
  https://www.dell.com/support/manuals/en-us/alienware-x17-r2-laptop/alienware-x17-r2-setup-and-specifications/keyboard-shortcuts-of-alienware-x17-r2?guid=guid-85b9eded-d823-4c96-a014-cba3bba5de1c&lang=en-us
- Dell, AWCC 6.x user guide: lighting, Go Light/Dark/Dim, key binds, store presets to device, stealth mode.
  https://www.dell.com/support/manuals/en-us/alienware-command-center/awcc_ug_6.x/lighting?guid=guid-a061d3ae-16e7-4f0d-a688-96913fec7992&lang=en-us
  https://www.dell.com/support/manuals/en-us/alienware-command-center/awcc_ug_6.x/create-a-key-bind?guid=guid-f37dbff6-a840-4e07-b059-487dcae3822e&lang=en-us
- alien-tools (T-Troll, MIT): x17 R2 = tron API v4, keyboard API v5; power-state slots 0x5B–0x60; effect
  sets; safety notes. https://github.com/T-Troll/alien-tools ,
  https://github.com/T-Troll/alienfx-tools/wiki/Supported-and-tested-devices-list
- Linux alienware-wmi: thermal and fans only, no lighting. https://docs.kernel.org/admin-guide/laptops/alienware-wmi.html
- Microsoft, Dynamic Lighting devices. https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/dynamic-lighting-devices
