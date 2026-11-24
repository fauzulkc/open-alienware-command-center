# Keyboard lighting control: the contract

Build spec for L1 + L2 of [lighting-and-macros-parity.md](lighting-and-macros-parity.md), after the L0 test
(2026-11-24). The backend (Rust: `crates/alienfx`, `crates/cockpit-core`, `crates/cli`, `app/src-tauri`) and
the UI (`app/ui`) are built against this file. Change it only together.

## Facts from L0 (do not re-derive)
- Keyboard: USB `0D62:CABC`, collection `FF89:00CC`, AlienFX API v5, 64-byte **feature** reports, report id
  0xCC. Sequences are in `alienfx::v5` (`apply_keys`, `brightness`, `global_effect`). Writes and the status
  read work without admin rights.
- **AWCC's LED ids are the keyboard's indices.** 91 keys; ids and names are in AWCC's FXMetadata and in
  `crates/alienfx/examples/write_test.rs` (`X17_KEYS`). AWCC's groups: Function Keys (12), Numbers (10),
  QWER (4), WASD (4).
- Zone controller `187C:0550` (API v4) accepts reports but its lights don't light on the owner's unit. The
  app offers zone colours as "sent, not visible on this laptop" and never relies on them.
- AWCC (`AlienFXSubAgent`, `FxDisplayService`) is still installed and running. It does **not** re-apply its
  look by itself after another program writes. Suspending both programs around a write (`NtSuspendProcess`,
  no admin) keeps one writer at a time.
- Owner rules: never lose the AWCC lighting config (never write AWCC's files; AWCC's look can always be put
  back from its own preset); never touch key input (Right Ctrl is the owner's Tab).

## Data model (Rust in `cockpit-core::lightctl`, serialised camelCase; TS mirrors it in `app/ui/src/api.ts`)

```ts
type Rgb = string;                       // "#RRGGBB"
type KeyEffect = 'static' | 'breathing' | 'wave' | 'dualWave' | 'pulse' | 'mixPulse' | 'nightRider' | 'laser';
interface KeyboardLight {
  on: boolean;                           // false = backlight off (brightness 0), colours kept
  brightness: number;                    // 0..100 (%), sent as v5 brightness 0..255 scaled
  effect: KeyEffect;                     // 'static' = per-key colours below; others = v5 global effects
  tempo: number;                         // 0..255, effects only (alien-tools' tempo byte)
  color: Rgb;                            // base colour for every key
  color2: Rgb;                           // second colour for dualWave / mixPulse
  groups: Record<string, Rgb>;           // group name (from `layout`) -> colour, over the base
  keys: Record<number, Rgb>;             // key id -> colour, over groups
}
interface LightingSettings {
  mode: 'awcc' | 'custom';               // 'awcc': the app never writes (AWCC's look stays); default 'awcc'
  keyboard: KeyboardLight;
  zones: Record<number, Rgb>;            // v4 light id -> colour (sent, not visible on this unit)
  offOnSleep: boolean;                   // dark before sleep, restored on wake        (default true)
  offOnLidClose: boolean;                // dark with the lid closed                   (default true)
  offOnDisplayOff: boolean;              // dark when the display turns off           (default true)
  dimOnBatterySaver: boolean;            // brightness 30% while Windows Energy Saver is on (default false)
  pauseAwcc: boolean;                    // suspend AWCC's lighting programs around each write (default true)
  ledBalance?: { r: number; g: number; b: number };   // LED colour balance, % per channel (default 100/100/100)
  ledPalette?: Record<Rgb, Rgb>;         // intended -> sent, matched by eye on these LEDs
  reactive?: ReactiveSettings;           // the reactive glow; missing in older files = defaults (off)
}
interface ReactiveSettings {             // cockpit-core::reactive, `lighting.reactive` in settings.json
  on: boolean;                           // run it in the app (custom + static only)              (default false)
  fps: number;                           // frames a second at most, 10..=30                       (default 18)
  decayMs: number;                       // the fade's time constant, 200..=3000 ms                (default 700)
  spread: number;                        // warmth a press adds to direct neighbours, 0..=1        (default 0.35)
  drift: number;                         // lean toward warm at the warm centre, 0..=0.5           (default 0.12)
  warm: Rgb;                             // intended warm colour (default #FF9480, sent #FF5030)
  peak: Rgb;                             // intended hint at a press's peak (default #FF6F61, sent #F83430)
}
// col = x position in key units (fractional), width in key units; every row is 16 units wide.
// present = false: an AWCC id with no physical key on this keyboard (JP1, JP3, UK1, JP6, JP4 and the
// second Page Up/Down, 113 and 115). Still written (restore and compose cover AWCC's 91), never drawn.
// The space bar has no LED and no id: the UI draws it unlit in the gap at row 5, x 4.25, width 5.
interface KeyInfo { id: number; name: string; row: number; col: number; width: number; group?: string; present: boolean }
interface LightingStatus {
  settings: LightingSettings;
  layout: KeyInfo[];                     // the 91 keys, in rows for the keyboard map (x17 R2 layout)
  groups: string[];                      // 'Function Keys', 'Numbers', 'QWER', 'WASD'
  awcc: { preset: string; keys: Record<number, Rgb> } | null;   // AWCC's active look, per key (read-only copy)
  device: { keyboard: boolean; zones: boolean; zonesVisible: false; error?: string };
  lastApply?: { ok: boolean; at: number; text: string };
}
```

Composition (pure, tested): per key `keys[id] ?? groups[groupOf(id)] ?? color`; `on=false` or a dark
trigger = brightness 0 with colours untouched; effects other than `static` use `color`/`color2`/`tempo`.

## App commands (Tauri)
| Command | Args | Returns |
|---|---|---|
| `lighting_status` | | `LightingStatus` |
| `lighting_set` | `settings: LightingSettings` | `LightingStatus` (validated, saved in Settings, applied when `mode='custom'`) |
| `lighting_preview_key` | `id: number, color: Rgb` | `LightingStatus` (one key, not saved; for the map's live preview) |
| `lighting_restore_awcc` | | `LightingStatus` (mode back to `awcc`, AWCC's preset written to the keyboard) |
| `lighting_import_awcc` | | `LightingStatus` (custom settings seeded from AWCC's preset, per key; not applied) |

Validation: brightness 0..=100, tempo 0..=255, colours `#RRGGBB`, key ids only the 91 known, group names only
the 4 known, zone ids only 0..=19; reactive fps 10..=30, decayMs 200..=3000, spread 0..=1, drift 0..=0.5,
warm and peak `#RRGGBB`. Refusals come back as an error string.

The reactive glow has no command of its own: `lighting_set` with `reactive.on` starts it (custom, static, lit)
and with it off stops it.

## CLI (`cockpit lighting …`, Windows only for writes; Linux: exit 2)
`show` (existing) · `status` · `color <#RRGGBB>` · `group <name> <#RRGGBB>` · `key <id|name> <#RRGGBB>` ·
`brightness <0-100>` · `off` · `on` · `effect <static|breathing|wave|…> [--tempo N] [--color2 #..]` ·
`restore-awcc` · `import-awcc` · `mode awcc|custom`. Each change saves the same settings the app uses.
`reactive [--seconds N]` runs the reactive glow in the foreground with the saved custom settings (they are used,
never changed; `reactive.on` is not needed): key presses read with Raw Input in that process, Ctrl+C or N seconds
stops it and writes the static colours back. Refused (exit 2) unless the mode is custom, the effect static and
the keyboard lit. `--dry-run` simulates typing (8 keys a second for 60 % of the time, then idle) on a fake
keyboard and prints the reports each second; it runs anywhere and sends nothing. Linux without `--dry-run`:
exit 2.

## Behaviour
- Every report passes `alienfx::allowed`; `is_memory_write` reports are never sent by this feature.
- A write goes: (pause AWCC if `pauseAwcc`) → reset → colours/brightness/effect → update → (resume AWCC).
  AWCC is always resumed, also on error (guard type).
- **Never hold AWCC paused for long.** Measured on the owner's x17 R2, 2026-11-24: with `AlienFXSubAgent` and
  `FxDisplayService` suspended for 40 s and nothing written, typing in Notepad was slow; before and after it was
  normal. The reactive glow and the keyboard clock (`cockpit lighting clock`) both held the pause for their
  whole run, and both made typing slow, the clock at only 5 reports a second. So a running animation must pause
  AWCC only around each write, or not at all, never for the run. Which of the two programs causes it is not
  yet known. The clock now pauses AWCC for each write only: run for 90 s on the owner's keyboard the same day,
  typing was normal and the seconds ticked cleanly with no blinking.
- Power: on suspend / lid close / display off (per settings) → brightness 0; on resume / lid open / display
  on → re-apply the current settings (AWCC may have written meanwhile). In `awcc` mode the app does nothing.
- `restore-awcc` writes AWCC's own preset (all 91 keys) and sets mode `awcc`.

## Keyboard clock (`cockpit-core::clock`, `cockpit lighting clock`)

The keyboard's chip has no clock and can't store a sequence of its own (v5: colours, brightness and the
built-in effects only), so the clock is driven from the PC, one change a second.

- The keyboard is a 16 x 5 grid (the F-row down to the Z row). `h:mm` in 3 x 5 digits on a dark keyboard,
  12-hour as Windows shows it here: the hour peach, a steady pink colon, the minutes cyan. For 5 s of every 15
  (from :10, :25, :40, :55) the seconds take over in big pink digits, with no colon. (A colon blinking every
  second read as colour flicker to the owner.)
- The digits use columns 0-12 (Esc to `=` on top), where every row has one key per column; the hour's tens
  (only ever a 1) is the left edge, Esc down to Left Shift. Wide keys stand for the leftmost column they cover
  at least half of, and Right Shift, which reaches past column 12, is lit at the share inside (43 %).
- Only changed keys are sent (none while h:mm shows until the minute turns, a digit's worth a second while the
  seconds show, up to about 50 when the view switches), and AWCC is
  paused around each write only. On stop the static colours are written back.
- Tried on the owner's keyboard, 2026-11-24: an analog face with hands from the centre couldn't be read; one
  key per digit row was rejected; the big digits over the dimmed preset were unclear (same hues behind them,
  and wide keys on the right filled the holes in the last digit). The version above, on a dark keyboard: typing
  normal, the time mostly clear, every digit in the rightmost spot clear.

## Mouse paint (`cockpit-core::mousepaint`, `cockpit lighting mouse`)

The whole screen (Windows' virtual screen, in real pixels: 3840 x 2160 on the owner's laptop) maps onto the
keyboard's 16 x 6 key grid, top-left = Esc, bottom-right = Right Arrow (the space bar has no light). The key
under the mouse pointer lights up and stays lit while the pointer is on it; when the pointer leaves, it fades
back over `--fade-ms` (default 2 s, eased so it dims quickly then lingers). Keys between two pointer readings
are lit too, so a fast swipe leaves no gaps.

- Looks (`--look`): `glow` (default: the saved colours stay, the mouse lights keys white), `uncover` (white
  keyboard, the mouse shows each key's own colour), `reveal` (dark keyboard, the mouse brings each key's own
  colour through), `boost` (the saved colours dimmed to `--dim` %, default 70, the owner's pick of 12-70 %; the mouse brings keys to full), `screen` (dark keyboard,
  the screen's colour under the pointer).
- The pointer is polled every 20 ms (`GetCursorPos`), no input hook; the screen's colour (one pixel,
  `GetPixel`) is read only for `screen`. Nothing is stored.
- At most `--fps` writes a second (default 10, 1-30), only changed keys, AWCC paused around each write only.
- Tried on the owner's keyboard, 2026-11-24: follows the mouse; the owner asked for the fading trail and the
  three looks, and likes all three.

## Screen mirror (`cockpit-core::screenmirror`, `cockpit lighting screen`)

The keyboard as an ambient light for the screen: one smooth gradient of the screen's colours over the whole
keyboard, all keys updated together.

- The screen is read small and split into 4 x 2 areas. `--reader gpu` (default) uses DXGI Desktop Duplication:
  the desktop image stays on the graphics card, which shrinks it by generating mipmaps, and only a 120 x 67
  level is copied back, and only when the screen changed. `--reader cpu` (and the fallback when Desktop
  Duplication can't start) reads it through GDI `StretchBlt`, point-sampled to 128 x 72.
  Each area's average colour, saturated (`--saturation`, default 2x), sits at its centre on the keyboard, and
  every key takes the bilinear gradient between the centres. `--spread` (0-1, default 1) pulls the areas
  toward the screen's average (0 = one colour for the whole keyboard).
- The whole keyboard eases toward the screen (`--smooth-ms`, default 300 ms) and is written all at once when
  any key moved more than 6 units, at most `--fps` times a second (default 8). AWCC paused around each write.
- Tried on the owner's keyboard, 2026-11-24, in steps: a per-key copy at 10 a second looked pale and
  flickered; saturation 2x fixed the paleness; a blurred per-key version with a 2 s ease was still distracting
  (bits changing on their own) and felt delayed; the whole-keyboard gradient "works as ambient", with spread 1.0
  (0.5 was "too much of the same").
- Cost, measured the same day on the owner's laptop (20 logical cores, 4K screen), 30 s each: the mirror about
  13 % of one core (0.66 % of the CPU) and 43 MB of RAM; with halftone shrinking at 64 x 36 it was 28 % of one
  core, so point sampling replaced it. With Desktop Duplication: about 3 % of one core (0.16 % of the CPU),
  reads of 6 ms at most, but about 215 MB of private memory committed by the graphics driver (39 MB in RAM),
  against 2 MB with GDI. The clock 0.7 % of one core and 11 MB; mouse paint 0.1 % of one core idle and 11 MB.

## Live lighting in the app (`cockpit-core::live`, `app/src-tauri/src/live.rs`)

The Lighting page is laid out in the owner's three layers of colour (2026-11-25), and the app keeps layers 2
and 3 running while it's open, tray included, drawn every frame (`live::run`; `cockpit lighting live` runs the
same in the foreground):

1. **Base profile**: the static per-key colours (the map, base colour, brightness).
2. **Effect**, one of: none, the **clock** or the **screen mirror** (drawn by the app, `kind` = off | clock |
   screen), or one of the keyboard's own effects (breathing, wave, ... `keyboard.effect`). The keyboard's own
   effects run on the chip, smooth, but in place of the per-key colours (one or two colours of their own), so
   layer 3 rests while one runs. Its Laser fires on key presses on the chip, but over one colour.
3. **Live interactions**:
   - **Key presses** (`tap`, any of): `laser` (a beam both ways along the pressed key's row), `lightning`
     (bolts in from both ends of the row, striking the key), `seismic` (a ring erupting over the whole
     keyboard, out to the corners in about 0.1 s and fading, in the pressed key's colour from the static
     profile). Laser and lightning use `tapColor` (or `keys`: each key's static colour), and always the keys'
     static colours over the screen mirror. `tapSpeed` keys a second (default 30), at most `tapFps` writes a
     second (12) while one runs. Key presses come from the app's read-only Raw Input listener (the boost
     key's), as LED ids only, delivered only while a run uses them; nothing records them.
   - **Mouse** (`mouse`): off, `glow` (a white trail), `screen` (a screen-colour trail), or `uncover` /
     `reveal` / `boost` (the profile shown white, dark or dimmed to `dim` %, the pointer bringing the static
     colours back where it goes; over the clock or mirror they bring the static colours through). The screen
     maps onto the keys (top-left = Esc), the trail fades over `fadeMs`. Settings saved before the layers kept
     this as `kind: mouse` with `look`; they still read that way.

`lighting.live` = `{ kind, look, fadeMs, dim, mouseFps, screenFps, smoothMs, spread, saturation, gpu, tap,
tapColor, tapSpeed, tapFps, mouse }`; missing fields take their defaults (nothing runs).
`clockOnDisplayOff` (Keep dark → When the display turns off: Stay lit | Go dark | Show the clock) shows the
clock alone while the display is off, instead of going dark (`live::plan`).

Tried on the owner's keyboard, 2026-11-25: automatic sweeps (laser, wave, back and forth over the static
colours) weren't wanted; the key-press laser was ("that's the one"); lightning came from the owner, as did
seismic, which first spread too slowly and now "erupts like a snap"; over the screen mirror the beams take the
static key colours; the seismic ring takes the pressed key's static colour.

- It runs only in custom mode, with the keyboard lit (not dark for sleep, lid or display) and the reactive
  glow off, when there is something to draw (`live::plan`). It replaces the keyboard's own effect while it
  runs. When the display turns off or on, or the keyboard goes dark, the plan changes and the run restarts
  with the new one.
- One thread runs it, with its own writes. Every other lighting write of the app (a save, a key preview, a power
  change, AWCC's look, quitting) first takes a hold (`lights::LIVE`): the animation stops with the static
  colours written back, the write happens, and the thread starts it again once nothing holds it. A hold is
  never waited on while holding the lighting lock (the thread takes that lock as it finishes).
- It also stops on its own, checked twice a second, when the saved settings change or it can't show any more.
- AWCC's lighting programs are paused around each of its writes only. A run that fails shows as the last
  apply and isn't retried for 30 s unless the settings change.

## Reactive glow (`cockpit-core::reactive`, `cockpit-core::keyinput`)

> **Tried on the owner's keyboard, 2026-11-24, and turned down.** This keyboard only shows new per-key colours
> after a reset, and a stream of frames that includes resets (18 a second with a status read, or 10 a second
> without) blinked, lagged behind presses and made typing itself slow. Without the reset (`light` frames) typing
> was normal but presses didn't show. So the app doesn't offer it; the code stays as an off-by-default experiment
> (`cockpit lighting reactive`, `lighting.reactive.frame` = `full` | `quick` | `light`). Hardware effects
> (breathing, wave) run on the keyboard itself and don't have this cost.
Each key press sends warmth into that key: it flares toward the warm colour, a little warmth spreads to its
neighbours, and it fades back to the key's own colour in about a second. A warm centre follows where the person
has been typing, and keys near it lean slightly warmer.

- **Model** (pure, times in ms): per-key heat 0..1. A press sets the key to 1 and adds `spread` to each key whose
  centre is within 1 key unit, falling off linearly to nothing at 1.5 units (layout `col`/`row`, key centres);
  heat never passes 1. Heat decays as exp(-t / `decayMs`), and under 0.001 it is 0. The warm centre is an
  exponential moving average of pressed key centres over 8 s; its weight rises 8 % of the way to 1 per press and
  decays as exp(-t / 10 s) (under 0.01 it is 0).
- **Frame**, in **sent (LED) space**: each key's sent colour (`LightingSettings::led` of the composed colour) is
  blended toward the sent warm colour by clamp(heat x 0.85 + drift), drift = `drift` x weight x
  max(0, 1 - distance to centre / 6 units). Above heat 0.8 the warm colour leans toward the peak colour, up to
  40 % at heat 1. Sent warm/peak = the `ledPalette` entry, else the owner's matches (#FF9480 -> #FF5030,
  #FF6F61 -> #F83430), else the LED balance. Idle (no heat, no weight) is exactly the static write's colours.
- **Awareness keys never change**: Esc 0, Backspace 35, Delete 15, Mic mute 19 (red risk) and Tab 40 (off).
  Ids with no key keep their colour.
- **Writes**: AWCC paused (when `pauseAwcc`) for the whole run, resumed on stop. First the full frame, exactly as
  a static write (`plan_from`: reset, status, 7 colorSets, loop, update = 11 reports). Then, at most `fps` times a
  second, only keys whose sent colour moved more than 3 units on a channel since last sent, largest first, at
  most 30 (two colorSets): reset, status, 1-2 colorSets, loop, update = 5-6 reports a frame, no brightness or
  effect reports. While no key is warm and only the drift fades: more than 6 units, at most every 500 ms. Once
  idle: any difference, so rest is exact; then nothing is sent. A failed write stops the glow and writes the static
  settings back; stopping writes them back the way `lightctl::write` does.
- **Load** (`cockpit lighting reactive --dry-run --seconds 120`, defaults, 8 keys a second): about 107 reports a
  second while typing (18 frames of 6, the cap); falling to 0 over about 4 s as the keys cool; then 0-10 a second
  (a small drift frame at most every 500 ms) for about 40 s while the warm centre fades; one last exact frame;
  then 0. Lower `fps` lowers the typing load in proportion (10 fps: at most 60 a second).
- **Key input** is read-only: Raw Input with `RIDEV_INPUTSINK` on the keyboard collection (the app shares the
  boost key's listener; the CLI opens its own). Nothing is blocked, injected or remapped (Right Ctrl as Tab
  through PowerToys passes untouched), and nothing logs or stores which keys were pressed: a key-down becomes an
  LED id and heat in memory, and is dropped at once while the glow isn't running. US layout, set-1 make codes with
  `RI_KEY_E0`; the Fn layer lights the physical key (Fn+Home Print Screen E0 37, Fn+End Pause E1 1D, Fn+Delete
  Insert E0 52, Fn+Up/Down Page Up/Down E0 49/E0 51, Fn+F1 boost 0x68). Fn alone, the space bar (no LED) and the
  media keys (speaker mute, volume, mic mute: consumer-control usages, not keyboard scancodes) produce nothing.
- **App**: the runner lives in the lighting runtime and its own thread writes each frame under the lighting lock.
  A settings write carries the glow on from the new static colours, or ends it when it can't run; restoring AWCC
  ends it. A dark trigger (sleep, lid, display off) writes brightness 0 and ends it; after the look is back on wake
  it starts again. Quitting stops it with the static colours written back. A start or frame that fails shows as
  the last apply and isn't retried until the settings change or another write succeeds.
