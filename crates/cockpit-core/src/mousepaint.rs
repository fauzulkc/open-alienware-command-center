//! Mouse paint: the screen mapped onto the keyboard, and the mouse pointer painting it. The key under the
//! pointer takes the brush's colour ([`Brush`]), and when the pointer moves on it fades back to the base over
//! `fade_ms` (default [`DEFAULT_FADE_MS`]; 0 keeps the paint), so moving the mouse leaves a trail.
//!
//! The owner's three looks (2026-11-24), as base + brush ([`look`]):
//! - glow (default): the saved colours stay (base `preset`) and the mouse lights keys white (brush `white`);
//! - uncover: the keyboard white (base `white`) and the mouse shows each key's own colour (brush `preset`);
//! - reveal: the keyboard dark (base `off`) and the mouse brings each key's own colour through (brush `preset`);
//! - boost: the saved colours dimmed (base `dim`, `dim_pct`) and the mouse brings each key to full (brush
//!   `preset`). The owner tried 12, 25, 40, 55 and 70 % (2026-11-24) and picked 70 ([`DEFAULT_DIM_PCT`]).
//!
//! Look `screen` paints the screen's colour under the pointer on a dark keyboard.
//!
//! - The whole screen (every monitor: Windows' virtual screen, in real pixels) maps onto the keyboard's 16 x 6
//!   key grid: left to right across the keys, top to bottom from the F-row to the bottom row. The space bar has
//!   no light, so the pointer paints nothing there.
//! - The pointer is read by polling its position ([`POLL_MS`]): no input hook, nothing in the way of the mouse
//!   or keyboard. The screen's colour is read at the pointer only when it moved, or every [`RESAMPLE_MS`].
//! - Keys between two readings are painted too (the path, sampled every quarter key), so a fast swipe leaves
//!   no gaps.
//! - Changes go out at most `fps` times a second, only the keys that changed, with AWCC paused around each
//!   write only (docs/lighting-control.md, "Never hold AWCC paused for long"). On stop the static colours are
//!   written back.
//!
//! Screen colours go through the LED balance and palette (`LightingSettings::led`), so white reads white.
//! Privacy: only the pointer's position and one pixel's colour are read, kept in memory, and never stored.

use crate::clock::{base_frame, send, ClockBase};
use crate::lightctl::{self, Applied, LightIo, LightingSettings, Mode, PowerState, Shown};
use crate::reactive::{delta, frame_steps, full_plan, mix, Clock, FrameKind};
use alienfx::device::{check_steps, Pauser};
use alienfx::v5::Key;
use alienfx::{x17, Api, Rgb};
use serde::Serialize;

/// How often the pointer is read, ms.
pub const POLL_MS: u64 = 20;
/// A still pointer's colour is read again this often, ms (the screen under it may change).
pub const RESAMPLE_MS: u64 = 250;
/// How long a painted key takes to fade back, ms (the owner asked for a slow trail, 2026-11-24).
pub const DEFAULT_FADE_MS: u64 = 2_000;
/// The fade's range: 0 (paint stays) to 10 s.
pub const FADE_RANGE: std::ops::RangeInclusive<u64> = 0..=10_000;
/// The dimmed base for look `boost`, % of the saved colours.
pub const DEFAULT_DIM_PCT: u32 = 70;
/// Writes a second, the range accepted.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=30;
/// What the mouse paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Brush {
    /// White (as the LEDs show white).
    #[default]
    White,
    /// Each key's own saved colour.
    Preset,
    /// The screen's colour under the pointer.
    Screen,
}

/// A look by name: (base, brush).
pub fn look(name: &str) -> Option<(ClockBase, Brush)> {
    match name.trim().to_ascii_lowercase().as_str() {
        "glow" => Some((ClockBase::Preset, Brush::White)),
        "uncover" => Some((ClockBase::White, Brush::Preset)),
        "reveal" => Some((ClockBase::Off, Brush::Preset)),
        "boost" => Some((ClockBase::Dim, Brush::Preset)),
        "screen" => Some((ClockBase::Off, Brush::Screen)),
        _ => None,
    }
}

/// The key grid: 16 keys across, 6 rows.
pub const GRID_W: f64 = 16.0;
pub const GRID_H: f64 = 6.0;

/// The screen area mapped onto the keyboard, pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Screen {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Screen {
    /// Pixel (`px`, `py`) as a share of the screen, 0..1 each way.
    pub fn share(&self, px: i32, py: i32) -> (f64, f64) {
        let f = |v: i32, o: i32, n: i32| ((v - o) as f64 / n.max(1) as f64).clamp(0.0, 0.999_999);
        (f(px, self.x, self.w), f(py, self.y, self.h))
    }
}

/// The pointer: where it is and the screen's colour there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    pub x: i32,
    pub y: i32,
    pub color: Rgb,
}

/// Where the pointer comes from: Windows, or a script in tests.
pub trait PointerSource {
    fn screen(&mut self) -> Screen;
    /// The pointer's position, `None` when it can't be read (a secure desktop, say).
    fn position(&mut self) -> Option<(i32, i32)>;
    /// The screen's colour at (`x`, `y`).
    fn color_at(&mut self, x: i32, y: i32) -> Option<Rgb>;
}

/// The lit key under (`fx`, `fy`), shares of the keyboard (0..1 across, 0..1 down); `None` where there is no
/// light (the space bar).
pub fn key_at(fx: f64, fy: f64) -> Option<u8> {
    let row = ((fy * GRID_H).floor().max(0.0) as u8).min(GRID_H as u8 - 1);
    let x = fx * GRID_W * 4.0; // quarter-key units, as the map
    x17::KEYS.iter().find(|k| k.present && k.row == row && (k.col as f64) <= x && x < (k.col + k.width) as f64).map(|k| k.id)
}

/// The keys along the path from `a` to `b` (shares of the keyboard), sampled every quarter key, in order,
/// each once.
pub fn keys_along(a: (f64, f64), b: (f64, f64)) -> Vec<u8> {
    let (dx, dy) = ((b.0 - a.0) * GRID_W, (b.1 - a.1) * GRID_H);
    let steps = ((dx.abs().max(dy.abs())) * 4.0).ceil().max(1.0) as usize;
    let mut out: Vec<u8> = Vec::new();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        if let Some(id) = key_at(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Why it can't run: it needs custom mode and a lit keyboard.
pub fn eligible(s: &LightingSettings, p: &PowerState, fps: u32, fade_ms: u64, dim_pct: u32) -> Result<(), String> {
    s.validate()?;
    if !(1..=100).contains(&dim_pct) {
        return Err(format!("a dimmed base of {dim_pct} % is out of range: 1-100"));
    }
    if !FPS_RANGE.contains(&fps) {
        return Err(format!("{fps} writes a second is out of range: 1-30"));
    }
    if !FADE_RANGE.contains(&fade_ms) {
        return Err(format!("a fade of {fade_ms} ms is out of range: 0-10000"));
    }
    if s.mode != Mode::Custom {
        return Err("lighting is AWCC's (mode awcc), so the app writes nothing: switch to custom for mouse paint".into());
    }
    if lightctl::level(s, p) == 0 {
        return Err("the keyboard is dark (off, brightness 0, or a sleep / lid / display rule)".into());
    }
    Ok(())
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintSummary {
    pub screen: Option<Screen>,
    /// Reports of the first, full picture.
    pub first: usize,
    /// Writes after it.
    pub frames: usize,
    pub reports: usize,
    pub keys: usize,
    pub ran_ms: u64,
    /// The write that put the static colours back.
    pub restore: Applied,
}

/// How much of a key's paint is left `age_ms` after the pointer left it: 1 down to 0 over `fade_ms`, eased
/// (squared) so it dims quickly at first and lingers faint, which reads as an even fade on these LEDs. A
/// `fade_ms` of 0 keeps the paint.
pub fn left(age_ms: u64, fade_ms: u64) -> f64 {
    if fade_ms == 0 {
        return 1.0;
    }
    let f = 1.0 - (age_ms as f64 / fade_ms as f64).min(1.0);
    f * f
}

/// The keyboard at `now`: each painted key between its paint and the base by [`left`]; keys whose paint is
/// gone are dropped from `paint`.
pub fn frame(base: &[(u8, Rgb)], paint: &mut Vec<(u8, Rgb, u64)>, now: u64, fade_ms: u64) -> Vec<(u8, Rgb)> {
    paint.retain(|(_, _, t)| fade_ms == 0 || now.saturating_sub(*t) < fade_ms);
    base.iter().map(|(id, b)| (*id, paint.iter().find(|(k, _, _)| k == id).map_or(*b, |(_, c, t)| mix(*b, *c, left(now.saturating_sub(*t), fade_ms))))).collect()
}

/// Runs mouse paint on `io` until `stop(now)` or the clock is stopped: the base first, then the keys the
/// pointer passed and their fading trail, at most `fps` writes a second. AWCC is paused around each write only. Stops cleanly: the
/// static settings written back, also after a failed write.
#[allow(clippy::too_many_arguments)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, base: ClockBase, brush: Brush, dim_pct: u32, fps: u32, fade_ms: u64, clock: &mut dyn Clock, src: &mut dyn PointerSource, stop: &mut dyn FnMut(u64) -> bool) -> Result<PaintSummary, String> {
    eligible(s, p, fps, fade_ms, dim_pct)?;
    let t0 = clock.now_ms();
    let screen = src.screen();
    let preset = base_frame(s, ClockBase::Preset);
    let base = match base {
        ClockBase::Dim => preset.iter().map(|(id, c)| (*id, mix(Rgb::new(0, 0, 0), *c, dim_pct as f64 / 100.0))).collect(),
        b => base_frame(s, b),
    };
    let white = s.led(Rgb::new(255, 255, 255));
    let mut shown = base.clone();
    let mut paint: Vec<(u8, Rgb, u64)> = Vec::new();
    let steps = full_plan(prev, &shown, s, p)?;
    check_steps(Api::V5, &steps)?;
    let mut dev = io.keyboard()?;
    let during = Shown { effect: false, level: lightctl::level(s, p) };
    let put_back = |io: &mut dyn LightIo| lightctl::write_from(io, during, s, p, pauser);
    let first = match send(dev.as_mut(), pauser, &steps) {
        Ok(n) => n,
        Err(e) => {
            drop(dev);
            let _ = put_back(io);
            return Err(format!("mouse paint couldn't start: {e}"));
        }
    };
    let mut sum = PaintSummary { screen: Some(screen), first, ..Default::default() };
    let interval = 1_000 / fps as u64;
    let mut next_write = t0;
    let mut last: Option<((i32, i32), (f64, f64))> = None;
    let mut last_sample = 0u64;
    let mut failed = None;
    loop {
        let now = clock.now_ms();
        if clock.stopped() || stop(now) {
            break;
        }
        if let Some(pos) = src.position() {
            let here = screen.share(pos.0, pos.1);
            let moved = last.is_none_or(|(p, _)| p != pos);
            // The screen's colour, read only for the screen brush, when the pointer moved or now and then.
            let sampled = if brush == Brush::Screen && (moved || now >= last_sample + RESAMPLE_MS) {
                last_sample = now;
                src.color_at(pos.0, pos.1).map(|c| s.led(c))
            } else {
                None
            };
            if moved || sampled.is_some() {
                let path = match last {
                    Some((_, from)) if moved => keys_along(from, here),
                    _ => key_at(here.0, here.1).into_iter().collect(),
                };
                for id in path {
                    let c = match brush {
                        Brush::White => white,
                        Brush::Preset => match preset.iter().find(|(k, _)| *k == id) {
                            Some((_, c)) => *c,
                            None => continue,
                        },
                        Brush::Screen => match sampled {
                            Some(c) => c,
                            None => continue,
                        },
                    };
                    // A move paints; a still pointer only updates the colour of a key still fading (the
                    // screen under it may change), so the last key fades like the rest of the trail.
                    match paint.iter_mut().find(|(k, _, _)| *k == id) {
                        Some(slot) => *slot = (id, c, if moved { now } else { slot.2 }),
                        None if moved => paint.push((id, c, now)),
                        None => {}
                    }
                }
            }
            last = Some((pos, here));
        }
        if now >= next_write {
            let want = frame(&base, &mut paint, now, fade_ms);
            let changed = delta(&shown, &want, 0);
            if !changed.is_empty() {
                let keys: Vec<Key> = changed.iter().map(|(id, c)| Key::new(*id, *c)).collect();
                match frame_steps(FrameKind::Full, &keys).and_then(|st| send(dev.as_mut(), pauser, &st)) {
                    Ok(n) => {
                        sum.frames += 1;
                        sum.reports += n;
                        sum.keys += changed.len();
                        shown = want;
                        next_write = now + interval;
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
        }
        clock.sleep_ms(POLL_MS);
    }
    sum.ran_ms = clock.now_ms() - t0;
    drop(dev);
    let restore = put_back(io);
    match (failed, restore) {
        (None, Ok(a)) => {
            sum.restore = a;
            Ok(sum)
        }
        (Some(e), Ok(a)) => Err(format!("Mouse paint stopped: {e}. The static colours were written back: {}", a.text())),
        (Some(e), Err(r)) => Err(format!("Mouse paint stopped: {e}. Writing the static colours back failed too: {r}")),
        (None, Err(r)) => Err(format!("mouse paint stopped, but writing the static colours back failed: {r}")),
    }
}

/// The real pointer and screen (Windows).
#[cfg(windows)]
pub mod win {
    use super::{PointerSource, Screen};
    use alienfx::Rgb;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID, HDC};
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};

    /// Reads the pointer and the screen in real pixels (the process is made DPI aware, so a 4K screen at 200 %
    /// is 3840 x 2160, and the pointer and the colour read agree).
    pub struct WinPointer {
        dc: HDC,
    }

    impl WinPointer {
        pub fn new() -> WinPointer {
            // SAFETY: plain Win32 calls; failing to set DPI awareness (already set) is harmless.
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                WinPointer { dc: GetDC(None) }
            }
        }
    }

    impl Default for WinPointer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for WinPointer {
        fn drop(&mut self) {
            // SAFETY: the DC from GetDC(None) in `new`.
            unsafe {
                ReleaseDC(None, self.dc);
            }
        }
    }

    impl PointerSource for WinPointer {
        fn screen(&mut self) -> Screen {
            // SAFETY: GetSystemMetrics takes no pointers.
            unsafe { Screen { x: GetSystemMetrics(SM_XVIRTUALSCREEN), y: GetSystemMetrics(SM_YVIRTUALSCREEN), w: GetSystemMetrics(SM_CXVIRTUALSCREEN), h: GetSystemMetrics(SM_CYVIRTUALSCREEN) } }
        }

        fn position(&mut self) -> Option<(i32, i32)> {
            let mut p = POINT::default();
            // SAFETY: `p` is a valid POINT.
            unsafe { GetCursorPos(&mut p) }.ok().map(|_| (p.x, p.y))
        }

        fn color_at(&mut self, x: i32, y: i32) -> Option<Rgb> {
            // SAFETY: the screen DC held for this source's life.
            let c = unsafe { GetPixel(self.dc, x, y) };
            (c.0 != CLR_INVALID).then(|| Rgb::new((c.0 & 0xFF) as u8, ((c.0 >> 8) & 0xFF) as u8, ((c.0 >> 16) & 0xFF) as u8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightctl::FakeIo;
    use alienfx::device::FakePauser;

    fn id(name: &str) -> u8 {
        x17::by_name(name).unwrap().id
    }

    #[test]
    fn the_screen_maps_onto_the_keys() {
        assert_eq!(key_at(0.0, 0.0), Some(id("ESC")));
        assert_eq!(key_at(0.999, 0.0), Some(id("Delete")));
        assert_eq!(key_at(0.999, 0.999), Some(id("Right Arrow")));
        assert_eq!(key_at(0.0, 0.999), Some(id("Left Ctrl")));
        assert_eq!(key_at(0.5, 0.999), None, "the space bar has no light");
        assert_eq!(key_at(0.5, 0.5), Some(id("J")), "the middle of the screen is J (the keyboard's centre is between H and J)");
        let s = Screen { x: 0, y: 0, w: 3840, h: 2160 };
        assert_eq!(s.share(0, 0), (0.0, 0.0));
        let (fx, fy) = s.share(3839, 2159);
        assert!(fx > 0.999 && fy > 0.999);
    }

    #[test]
    fn a_swipe_paints_every_key_it_crosses() {
        let row: Vec<u8> = keys_along((0.0, 0.05), (0.999, 0.05));
        let f_row: Vec<u8> = x17::KEYS.iter().filter(|k| k.present && k.row == 0).map(|k| k.id).collect();
        assert_eq!(row, f_row, "left to right over the F-row");
        let down = keys_along((0.03, 0.0), (0.03, 0.999));
        assert_eq!(down, [id("ESC"), id("`"), id("Tab"), id("Caps Lock"), id("Left Shift"), id("Left Ctrl")]);
    }

    /// A pointer that sweeps left to right along the home row over 2 s; the screen is red on the left half,
    /// blue on the right.
    struct Sweep(u64);
    impl PointerSource for Sweep {
        fn screen(&mut self) -> Screen {
            Screen { x: 0, y: 0, w: 1600, h: 600 }
        }
        fn position(&mut self) -> Option<(i32, i32)> {
            self.0 += 1;
            Some((((self.0 * POLL_MS).min(2_000) as f64 / 2_000.0 * 1599.0) as i32, 350))
        }
        fn color_at(&mut self, x: i32, _y: i32) -> Option<Rgb> {
            Some(if x < 800 { Rgb::new(255, 0, 0) } else { Rgb::new(0, 0, 255) })
        }
    }

    struct Watch(u64, FakePauser);
    impl Clock for Watch {
        fn now_ms(&mut self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            assert_eq!(self.1.paused(), self.1.resumed(), "AWCC resumed before every wait");
            self.0 += ms;
        }
    }

    #[test]
    fn the_trail_fades_back_to_the_base() {
        assert_eq!((left(0, 2_000), left(2_000, 2_000), left(5_000, 0)), (1.0, 0.0, 1.0));
        assert!((left(1_000, 2_000) - 0.25).abs() < 1e-9, "eased: a quarter left halfway");
        let base = vec![(1u8, Rgb::new(0, 0, 0)), (2, Rgb::new(0, 0, 0))];
        let mut paint = vec![(1u8, Rgb::new(200, 0, 0), 0u64)];
        assert_eq!(frame(&base, &mut paint, 0, 2_000)[0].1, Rgb::new(200, 0, 0));
        assert_eq!(frame(&base, &mut paint, 1_000, 2_000)[0].1, Rgb::new(50, 0, 0));
        assert_eq!(frame(&base, &mut paint, 2_000, 2_000)[0].1, Rgb::new(0, 0, 0), "back to the base");
        assert!(paint.is_empty(), "faded paint is dropped");
    }

    #[test]
    fn a_trail_keeps_writing_until_it_has_faded() {
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let p = PowerState::default();
        let io = FakeIo::default();
        let pz = FakePauser::default();
        let mut w = Watch(0, pz.clone());
        // The sweep ends at 2 s; with a 1 s fade the last writes are by about 3 s, then nothing.
        let sum = run(&mut io.clone(), &s, &p, Shown::of(&s, &p), Some(&pz), ClockBase::Off, Brush::Screen, DEFAULT_DIM_PCT, 10, 1_000, &mut w, &mut Sweep(0), &mut |now| now >= 6_000).unwrap();
        assert!(sum.frames > 20 && sum.frames <= 32, "writes while moving and fading, then none: {}", sum.frames);
    }

    #[test]
    fn the_looks_paint_with_white_or_the_keys_own_colours() {
        // The default look is AWCC's blue on every key: neither white nor dark, so each look changes J.
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let p = PowerState::default();
        let j = id("J");
        let own = base_frame(&s, ClockBase::Preset).iter().find(|(k, _)| *k == j).unwrap().1;
        assert!(own != s.led(Rgb::new(255, 255, 255)) && own != Rgb::new(0, 0, 0));
        // A pointer resting in the middle of the screen (J) for 1 s, no fade.
        struct Rest;
        impl PointerSource for Rest {
            fn screen(&mut self) -> Screen {
                Screen { x: 0, y: 0, w: 1600, h: 600 }
            }
            fn position(&mut self) -> Option<(i32, i32)> {
                Some((800, 300))
            }
            fn color_at(&mut self, _x: i32, _y: i32) -> Option<Rgb> {
                Some(Rgb::new(255, 0, 0))
            }
        }
        for name in ["glow", "uncover", "reveal", "boost"] {
            let (base, brush) = look(name).unwrap();
            let io = FakeIo::default();
            let mut w = Watch(0, FakePauser::default());
            let sum = run(&mut io.clone(), &s, &p, Shown::of(&s, &p), None, base, brush, DEFAULT_DIM_PCT, 10, 0, &mut w, &mut Rest, &mut |now| now >= 1_000).unwrap();
            assert_eq!((sum.frames, sum.keys), (1, 1), "{name}: one write, only J");
        }
    }

    #[test]
    fn a_sweep_paints_the_row_at_the_capped_rate() {
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let p = PowerState::default();
        let io = FakeIo::default();
        let pz = FakePauser::default();
        let mut w = Watch(0, pz.clone());
        let sum = run(&mut io.clone(), &s, &p, Shown::of(&s, &p), Some(&pz), ClockBase::Off, Brush::Screen, DEFAULT_DIM_PCT, 10, 0, &mut w, &mut Sweep(0), &mut |now| now >= 3_000).unwrap();
        assert!(sum.frames <= 21, "at most 10 writes a second over 2 s of movement: {}", sum.frames);
        assert!(sum.keys >= 13, "the home row painted: {} key colours", sum.keys);
        assert_eq!(pz.paused(), pz.resumed());
        assert!(sum.restore.keyboard > 0, "static colours written back");
    }
}
