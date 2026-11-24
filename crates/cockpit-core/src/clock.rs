//! The keyboard clock: the local time drawn in big digits across the whole keyboard.
//!
//! The keyboard is used as a 16 x 5 grid of pixels (the F-row down to the Z row; every row is 16 keys wide):
//! `h:mm` in 3 x 5 digits, 12-hour as Windows shows it here, the hour peach and the minutes cyan, a steady pink
//! colon between them (a blinking one read as flicker to the owner, 2026-11-24). For 5 seconds out of every 15
//! ([`shows_seconds`]) the seconds take over: `ss` in big pink digits on the right, ticking, no colon.
//!
//! The rows are staggered, so each key stands for the grid column under its centre, nudged a quarter key to
//! the left (on the QWERTY and home rows that lines the letters up under the number row best), and a wide key
//! for the leftmost column it covers at least half of. Columns 0-12 have one key each on every row (Esc to
//! `=` on top), so the time is drawn there: a wide key covering two drawn columns filled the holes in the
//! digits (a 0 read as an 8, 2026-11-24). The hour's tens (only ever a 1) is the left edge, Esc down to Left
//! Shift; then the hour's units at columns 2-4, the colon at 5, the minutes at 6-8 and 10-12.
//!
//! The rest of the keyboard is the base: dark ([`ClockBase::Off`], default: the owner couldn't read the digits
//! over the dimmed colours, 2026-11-24), the saved static colours dimmed ([`ClockBase::Dim`]) or as they are
//! ([`ClockBase::Preset`]). Only the keys that change are sent: nothing while h:mm shows (once a minute), a
//! digit's worth a second while the seconds show. AWCC is paused around each write only, never for the run: holding it paused makes
//! typing slow (docs/lighting-control.md). On stop the static settings are written back.

use crate::lightctl::{self, compose, parse_rgb, Applied, LightIo, LightingSettings, Mode, PowerState, Shown};
use crate::reactive::{delta, frame_steps, full_plan, mix, sent, Clock, FrameKind};
use alienfx::device::{check_steps, run_steps, LightDevice, Pauser};
use alienfx::v5::{Key, V5Step};
use alienfx::{x17, Api, Rgb};
use serde::Serialize;

/// The hour colour (intended): the owner's peach.
pub const HOUR: &str = "#FF9480";
/// The minute colour (intended): the owner's cyan.
pub const MINUTE: &str = "#00E5FF";
/// The colon and the seconds (intended): the owner's pink.
pub const SECOND: &str = "#FF3DB4";
/// The dimmed base: the static colours at this share.
pub const DIM: f64 = 0.12;
/// How often the time is looked at, ms (a change goes out within this of the second turning).
pub const POLL_MS: u64 = 50;
/// The grid: 16 columns, the top 5 rows of keys.
pub const COLS: usize = 16;
pub const ROWS: usize = 5;

/// 3 x 5 digits, top row first; bit 2 is the left column.
const FONT: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b001, 0b001], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

/// Where the digits start: hour units, minute tens, minute units; the colon's column; the hour's tens (a 1,
/// one column wide).
const DIGIT_AT: [usize; 3] = [2, 6, 10];
const COLON_AT: usize = 5;
const TENS_AT: usize = 0;

/// What the keys that aren't showing the time look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockBase {
    Dim,
    #[default]
    Off,
    Preset,
    /// Every key white (as the LEDs show white: the owner's palette).
    White,
}

impl ClockBase {
    pub fn parse(s: &str) -> Option<ClockBase> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dim" => Some(ClockBase::Dim),
            "off" | "dark" => Some(ClockBase::Off),
            "preset" | "static" => Some(ClockBase::Preset),
            "white" => Some(ClockBase::White),
            _ => None,
        }
    }
}

/// A time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Hms {
    pub h: u8,
    pub m: u8,
    pub s: u8,
}

impl Hms {
    pub fn from_secs(day_secs: u64) -> Hms {
        let t = day_secs % 86_400;
        Hms { h: (t / 3_600) as u8, m: (t / 60 % 60) as u8, s: (t % 60) as u8 }
    }

    /// The hour on a 12-hour clock: 1..=12.
    pub fn h12(&self) -> u8 {
        match self.h % 12 {
            0 => 12,
            x => x,
        }
    }
}

/// The local time now (Windows); UTC elsewhere.
pub fn local_now() -> Hms {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        Hms { h: t.wHour as u8, m: t.wMinute as u8, s: t.wSecond as u8 }
    }
    #[cfg(not(windows))]
    {
        Hms::from_secs(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
    }
}

/// The grid cell a key stands for, (row, column): `None` for keys below the grid or not on this keyboard. A
/// one-unit key stands for the column under its centre nudged a quarter key left; a wide key (Tab, Caps Lock,
/// the Shifts, Enter, Backspace, the | key) for the leftmost column it covers at least half of.
pub fn cell(k: &x17::KeyDef) -> Option<(usize, usize)> {
    if !k.present || k.row as usize >= ROWS {
        return None;
    }
    let (l, w) = (k.col as f64 / 4.0, k.width as f64 / 4.0);
    let col = if k.width <= 4 {
        (l + w / 2.0 - 0.25).floor().max(0.0) as usize
    } else {
        (0..COLS).find(|&c| (l + w).min(c as f64 + 1.0) - l.max(c as f64) >= 0.5).unwrap_or(0)
    };
    Some((k.row as usize, col.min(COLS - 1)))
}

/// What a pixel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Px {
    Off,
    Hour,
    Minute,
    Colon,
    Second,
}

/// The seconds take over for 5 s out of every 15 (from :10, :25, :40 and :55).
pub fn shows_seconds(t: Hms) -> bool {
    t.s % 15 >= 10
}

/// The 16 x 5 picture of `t`.
pub fn pixels(t: Hms) -> [[Px; COLS]; ROWS] {
    let mut g = [[Px::Off; COLS]; ROWS];
    let h = t.h12();
    let seconds = shows_seconds(t);
    let digits = if seconds {
        [(0, Px::Off, false), (t.s / 10, Px::Second, true), (t.s % 10, Px::Second, true)]
    } else {
        if h >= 10 {
            for row in g.iter_mut() {
                row[TENS_AT] = Px::Hour;
            }
        }
        [(h % 10, Px::Hour, true), (t.m / 10, Px::Minute, true), (t.m % 10, Px::Minute, true)]
    };
    for (i, (d, px, shown)) in digits.into_iter().enumerate() {
        if !shown {
            continue;
        }
        for (r, bits) in FONT[d as usize].iter().enumerate() {
            for c in 0..3 {
                if bits & (0b100 >> c) != 0 {
                    g[r][DIGIT_AT[i] + c] = px;
                }
            }
        }
    }
    if !seconds {
        g[1][COLON_AT] = Px::Colon;
        g[3][COLON_AT] = Px::Colon;
    }
    g
}

/// The base (sent colours, every key the static settings set).
pub fn base_frame(s: &LightingSettings, base: ClockBase) -> Vec<(u8, Rgb)> {
    compose(s)
        .into_iter()
        .map(|(id, c)| {
            let c = s.led(c);
            let c = match base {
                ClockBase::Preset => c,
                ClockBase::Dim => mix(Rgb::new(0, 0, 0), c, DIM),
                ClockBase::Off => Rgb::new(0, 0, 0),
                ClockBase::White => s.led(Rgb::new(255, 255, 255)),
            };
            (id, c)
        })
        .collect()
}

/// The right edge of the drawn columns (0-12), key units.
pub const DRAWN_RIGHT: f64 = 13.0;

/// The share of key `k` inside the drawn columns: 1 for every key but Right Shift, which reaches from column
/// 12 well past it (lit fully, the bottom of the last digit stuck out to the right: the owner couldn't read a
/// 5 there, 2026-11-24), so it shows at that share.
pub fn inside(k: &x17::KeyDef) -> f64 {
    let (l, w) = (k.col as f64 / 4.0, k.width as f64 / 4.0);
    // One-unit keys stand for their column whole (`]` reaches half a key past column 12 by the stagger).
    if k.width <= 4 || l + w <= DRAWN_RIGHT {
        return 1.0;
    }
    ((DRAWN_RIGHT - l) / w).clamp(0.0, 1.0)
}

/// The whole keyboard at `t` (sent colours): the base with the digits and the colon on top.
pub fn draw(s: &LightingSettings, base: &[(u8, Rgb)], t: Hms) -> Vec<(u8, Rgb)> {
    let c = |hex: &str| sent(s, parse_rgb(hex).expect("clock colours are valid"));
    let (hour, minute, second) = (c(HOUR), c(MINUTE), c(SECOND));
    let g = pixels(t);
    base.iter()
        .map(|(id, b)| {
            let k = x17::KEYS.iter().find(|k| k.id == *id);
            let px = k.and_then(cell).map_or(Px::Off, |(row, col)| g[row][col]);
            let colour = match px {
                Px::Hour => hour,
                Px::Minute => minute,
                Px::Colon | Px::Second => second,
                Px::Off => return (*id, *b),
            };
            (*id, mix(*b, colour, k.map_or(1.0, inside)))
        })
        .collect()
}

/// Why the clock can't run: it needs custom mode and a lit keyboard.
pub fn eligible(s: &LightingSettings, p: &PowerState) -> Result<(), String> {
    s.validate()?;
    if s.mode != Mode::Custom {
        return Err("lighting is AWCC's (mode awcc), so the app writes nothing: switch to custom for the clock".into());
    }
    if lightctl::level(s, p) == 0 {
        return Err("the keyboard is dark (off, brightness 0, or a sleep / lid / display rule)".into());
    }
    Ok(())
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockSummary {
    /// Reports of the first, full picture.
    pub first: usize,
    /// Changes sent after it.
    pub frames: usize,
    pub reports: usize,
    pub keys: usize,
    pub ran_ms: u64,
    /// The write that put the static colours back.
    pub restore: Applied,
}

/// `steps` sent with AWCC paused (when `pauser` is given) for that write only, resumed as soon as it's done:
/// holding AWCC's lighting programs suspended for longer makes typing slow on the owner's keyboard
/// (docs/lighting-control.md, "Never hold AWCC paused for long").
pub(crate) fn send(dev: &mut dyn LightDevice, pauser: Option<&dyn Pauser>, steps: &[V5Step]) -> Result<usize, String> {
    let _guard = pauser.map(|pz| pz.pause().map_err(|e| format!("couldn't pause AWCC: {e}; nothing was sent"))).transpose()?;
    run_steps(dev, Api::V5, steps).map(|log| log.sent)
}

/// Runs the clock on `io` until `stop(now)` or the clock is stopped: the whole picture first, then the changed
/// keys whenever `now_hms()` moves. AWCC is paused around each write only (when `pauser` is given), never for
/// the run. Stops cleanly: the static settings written back, also after a failed write.
#[allow(clippy::too_many_arguments)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, base: ClockBase, clock: &mut dyn Clock, now_hms: &mut dyn FnMut() -> Hms, stop: &mut dyn FnMut(u64) -> bool) -> Result<ClockSummary, String> {
    eligible(s, p)?;
    let t0 = clock.now_ms();
    let base = base_frame(s, base);
    let mut shown = draw(s, &base, now_hms());
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
            return Err(format!("the clock couldn't start: {e}"));
        }
    };
    let mut sum = ClockSummary { first, ..Default::default() };
    let mut failed = None;
    loop {
        let now = clock.now_ms();
        if clock.stopped() || stop(now) {
            break;
        }
        let next = draw(s, &base, now_hms());
        let changed = delta(&shown, &next, 0);
        if !changed.is_empty() {
            let keys: Vec<Key> = changed.iter().map(|(id, c)| Key::new(*id, *c)).collect();
            match frame_steps(FrameKind::Full, &keys).and_then(|st| send(dev.as_mut(), pauser, &st)) {
                Ok(n) => {
                    sum.frames += 1;
                    sum.reports += n;
                    sum.keys += changed.len();
                    shown = next;
                }
                Err(e) => {
                    failed = Some(e);
                    break;
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
        (Some(e), Ok(a)) => Err(format!("The clock stopped: {e}. The static colours were written back: {}", a.text())),
        (Some(e), Err(r)) => Err(format!("The clock stopped: {e}. Writing the static colours back failed too: {r}")),
        (None, Err(r)) => Err(format!("the clock stopped, but writing the static colours back failed: {r}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightctl::FakeIo;
    use alienfx::device::FakePauser;

    fn custom() -> LightingSettings {
        LightingSettings { mode: Mode::Custom, ..Default::default() }
    }

    /// The keyboard as text: each key at its place, `H`/`M`/`:` for the digits and colon, `s` for the bar.
    fn picture(t: Hms) -> String {
        let s = custom();
        let f = draw(&s, &base_frame(&s, ClockBase::Off), t);
        let c = |hex: &str| sent(&s, parse_rgb(hex).unwrap());
        let mut rows = vec![vec![' '; 66]; 6];
        for k in x17::KEYS.iter().filter(|k| k.present) {
            let col = f.iter().find(|(i, _)| *i == k.id).map(|(_, c)| *c).unwrap_or_default();
            let ch = if col == c(HOUR) {
                'H'
            } else if col == c(MINUTE) {
                'M'
            } else if col == c(SECOND) {
'S'
            } else {
                '.'
            };
            for x in k.col as usize..(k.col + k.width) as usize {
                rows[k.row as usize][x] = if x == k.col as usize { '|' } else { ch };
            }
        }
        rows.into_iter().map(|r| r.into_iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn every_drawn_cell_has_exactly_one_key() {
        for row in 0..ROWS {
            for col in 0..=12 {
                let n = x17::KEYS.iter().filter(|k| cell(k) == Some((row, col))).count();
                let gap = (row == 3 || row == 4) && col == 1; // under Caps Lock / Left Shift: a blank column anyway
                assert_eq!(n, usize::from(!gap), "row {row} column {col}: {n} keys");
            }
        }
        let partial: Vec<(&str, u32)> = x17::KEYS.iter().filter(|k| cell(k).is_some_and(|c| c.1 <= 12) && inside(k) < 1.0).map(|k| (k.name, (inside(k) * 100.0).round() as u32)).collect();
        assert_eq!(partial, [("Right Shift", 43)], "only Right Shift reaches past the drawn columns");
        let tens: Vec<&str> = x17::KEYS.iter().filter(|k| cell(k).is_some_and(|c| c.1 == TENS_AT)).map(|k| k.name).collect();
        assert_eq!(tens, ["ESC", "`", "Tab", "Caps Lock", "Left Shift"]);
    }

    #[test]
    fn the_time_reads_in_big_digits() {
        for t in [Hms { h: 17, m: 42, s: 8 }, Hms { h: 10, m: 59, s: 31 }, Hms { h: 0, m: 5, s: 0 }, Hms { h: 18, m: 48, s: 0 }, Hms { h: 18, m: 49, s: 0 }, Hms { h: 18, m: 50, s: 0 }, Hms { h: 18, m: 50, s: 57 }] {
            println!("{}:{:02}:{:02}\n{}\n", t.h12(), t.m, t.s, picture(t));
        }
        let g = pixels(Hms { h: 17, m: 42, s: 8 });
        // 5:42 -> no hour tens; a 5 at column 2, a 4 at 6, a 2 at 10; the colon on (even second).
        assert!(g.iter().all(|r| r[TENS_AT] == Px::Off), "no leading 1");
        assert_eq!(g[0][2..5], [Px::Hour; 3], "top of the 5");
        assert_eq!((g[0][6], g[0][7], g[0][8]), (Px::Minute, Px::Off, Px::Minute), "top of the 4");
        assert_eq!(g[4][10..13], [Px::Minute; 3], "bottom of the 2");
        assert!(g.iter().all(|r| r[13..].iter().all(|p| *p == Px::Off)), "the right edge stays dark");
        assert_eq!((g[1][COLON_AT], g[3][COLON_AT]), (Px::Colon, Px::Colon));
        assert_eq!(pixels(Hms { h: 17, m: 42, s: 9 }), g, "nothing changes from one second to the next (steady colon)");
        assert!(pixels(Hms { h: 0, m: 0, s: 0 }).iter().all(|r| r[TENS_AT] == Px::Hour), "12 o'clock has a 1");
    }

    #[test]
    fn the_seconds_take_over_five_of_every_fifteen() {
        let shown: Vec<u8> = (0..60).filter(|&sec| shows_seconds(Hms { h: 9, m: 0, s: sec })).collect();
        assert_eq!(shown, [10, 11, 12, 13, 14, 25, 26, 27, 28, 29, 40, 41, 42, 43, 44, 55, 56, 57, 58, 59]);
        let g = pixels(Hms { h: 9, m: 7, s: 57 });
        assert!(g.iter().all(|r| r[..COLON_AT].iter().all(|p| *p == Px::Off)), "the left half is dark");
        assert_eq!(g[0][6..9], [Px::Second; 3], "top of the 5");
        assert!(!g.iter().flatten().any(|p| *p == Px::Colon), "no colon with the seconds");
        assert!(!g.iter().flatten().any(|p| matches!(p, Px::Hour | Px::Minute)));
    }

    #[test]
    fn a_second_changes_few_keys() {
        let s = custom();
        let base = base_frame(&s, ClockBase::Dim);
        let mut most = 0;
        for sec in 0..3_600u64 {
            let a = draw(&s, &base, Hms::from_secs(10 * 3_600 + sec));
            let b = draw(&s, &base, Hms::from_secs(10 * 3_600 + sec + 1));
            most = most.max(delta(&a, &b, 0).len());
        }
        println!("most keys changed in a second: {most}");
        assert!(most <= 60, "a switch between h:mm and :ss stays a few colour reports: {most} keys");
    }

    struct Sim(u64);
    impl Clock for Sim {
        fn now_ms(&mut self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.0 += ms;
        }
    }

    #[test]
    fn a_run_sends_only_when_the_picture_changes_and_puts_the_colours_back() {
        let s = custom();
        let p = PowerState::default();
        let io = FakeIo::default();
        let mut sim = Sim(0);
        let start = 17 * 3_600 + 42 * 60 + 5;
        let mut t = 0u64;
        let sum = run(&mut io.clone(), &s, &p, Shown::of(&s, &p), None, ClockBase::Dim, &mut sim, &mut || {
            t += POLL_MS;
            Hms::from_secs(start + t / 1_000)
        }, &mut |now| now >= 10_000)
        .unwrap();
        // 5:42:05 to :15: h:mm holds still to :09, then :10 (switch to seconds), :11 to :14, :15 (switch back).
        assert_eq!(sum.frames, 6, "changes only when the picture changes");
        assert!(sum.restore.keyboard > 0, "static colours written back");
    }

    /// Sleeps check that AWCC isn't held between writes.
    struct Watch(u64, FakePauser, u32);
    impl Clock for Watch {
        fn now_ms(&mut self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            assert_eq!(self.1.paused(), self.1.resumed(), "AWCC resumed before every wait");
            self.2 += 1;
            self.0 += ms;
        }
    }

    #[test]
    fn awcc_is_paused_around_each_write_only() {
        let s = custom();
        let p = PowerState::default();
        let io = FakeIo::default();
        let pz = FakePauser::default();
        let mut w = Watch(0, pz.clone(), 0);
        let mut t = 0u64;
        let sum = run(&mut io.clone(), &s, &p, Shown::of(&s, &p), Some(&pz), ClockBase::Dim, &mut w, &mut || {
            t += POLL_MS;
            Hms::from_secs(3_600 + t / 1_000)
        }, &mut |now| now >= 5_000)
        .unwrap();
        assert!(w.2 > 50, "waited between polls");
        assert_eq!(pz.paused(), pz.resumed(), "resumed at the end");
        assert!(pz.paused() as usize >= sum.frames + 2, "one pause per write: first, {} changes, restore", sum.frames);
    }
}
