//! Live lighting in the app: PC-drawn lighting kept running while the app is open (`lighting.live` in
//! settings.json), in the owner's three layers (2026-11-25), drawn every frame:
//!
//! 1. The base profile: the static per-key colours (shown white, dark or dimmed for the mouse's uncover,
//!    reveal and boost).
//! 2. The effect ([`LiveKind`]): none, the keyboard clock or the screen mirror. The keyboard's own effects
//!    (breathing, wave...) are the other choices here, but they run on the chip in place of the per-key
//!    colours, so nothing of this runs with them.
//! 3. Live interactions: key presses ([`TapKind`]: a laser along the pressed key's row, lightning in to it, a
//!    seismic ring erupting from it) and the mouse ([`LiveSettings::mouse`]: a white or screen-colour trail,
//!    or uncover / reveal / boost, which show the static colours where the pointer goes). Key presses come
//!    from a read-only listener (Raw Input); nothing records which keys were pressed.
//!
//! When the display turns off and `clockOnDisplayOff` is set, the base is the clock and the layers rest
//! ([`plan`]). The pure parts (the plan, the beams, a frame) are tested here; [`run`] is the loop, with the
//! clock, pointer, screen and key presses given ([`Sources`]), and [`run_windows`] opens the real ones.
//!
//! Only changed keys are written, at most as often as the fastest layer needs; AWCC is paused around each
//! write only, never for the run (holding it makes typing slow). On stop the static colours come back.

use crate::clock::{self, base_frame, send, ClockBase, Hms};
use crate::lightctl::{self, parse_rgb, LightIo, LightingSettings, Mode, PowerState, Shown};
use crate::mousepaint::{self, Brush, PointerSource};
use crate::reactive::{delta, diff, frame_steps, full_plan, key_centre, mix, Clock, FrameKind};
use crate::screenmirror::{self, ScreenSource};
use alienfx::device::{check_steps, Pauser};
use alienfx::v5::Key;
use alienfx::{x17, Api, Rgb};
use serde::{Deserialize, Serialize};

/// The base layer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LiveKind {
    /// No effect: the base profile (nothing runs unless an interaction is on).
    #[default]
    Off,
    /// The keyboard clock: h:mm in big digits, the seconds for 5 s of every 15.
    Clock,
    /// Settings saved before the three layers: no effect, with `look` as the mouse interaction.
    Mouse,
    /// The screen mirror: one gradient of the screen's colours over the whole keyboard.
    Screen,
}

impl LiveKind {
    pub const ALL: [LiveKind; 4] = [LiveKind::Off, LiveKind::Clock, LiveKind::Mouse, LiveKind::Screen];

    pub fn name(self) -> &'static str {
        match self {
            LiveKind::Off => "off",
            LiveKind::Clock => "clock",
            LiveKind::Mouse => "mouse",
            LiveKind::Screen => "screen",
        }
    }

    pub fn parse(s: &str) -> Option<LiveKind> {
        LiveKind::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// An effect on key presses, drawn over the base. Any of them can be on together (`lighting.live.tap`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TapKind {
    /// A beam from the pressed key, both ways along its row.
    Laser,
    /// Lightning in from both ends of the row, striking the pressed key (the owner's idea, 2026-11-25).
    Lightning,
    /// Rings erupting over the whole keyboard from the pressed key, in that key's colour in the static profile,
    /// whatever the base shows (the owner, 2026-11-25).
    Seismic,
}

impl TapKind {
    pub const ALL: [TapKind; 3] = [TapKind::Laser, TapKind::Lightning, TapKind::Seismic];

    pub fn parse(s: &str) -> Option<TapKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "laser" => Some(TapKind::Laser),
            "lightning" => Some(TapKind::Lightning),
            "seismic" => Some(TapKind::Seismic),
            _ => None,
        }
    }
}

/// Which key-press effects are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Taps {
    pub laser: bool,
    pub lightning: bool,
    pub seismic: bool,
}

impl Taps {
    pub fn of(kinds: &[TapKind]) -> Taps {
        Taps { laser: kinds.contains(&TapKind::Laser), lightning: kinds.contains(&TapKind::Lightning), seismic: kinds.contains(&TapKind::Seismic) }
    }
    pub fn any(&self) -> bool {
        self.laser || self.lightning || self.seismic
    }
}

/// `tapColor` for each key's own static colour.
pub const KEY_COLOURS: &str = "keys";

/// The looks mouse paint accepts ([`mousepaint::look`]).
pub const LOOKS: [&str; 5] = ["glow", "uncover", "reveal", "boost", "screen"];
/// The mouse interactions: off, a white trail (`glow`), a screen-colour trail (`screen`), or the static colours
/// shown where the pointer goes over a white (`uncover`), dark (`reveal`) or dimmed (`boost`) keyboard.
pub const MOUSE_MODES: [&str; 6] = ["off", "glow", "screen", "uncover", "reveal", "boost"];

/// The live layers and their options (`lighting.live`). Defaults are what the owner picked on the keyboard
/// (2026-11-24); a missing field takes its default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LiveSettings {
    pub kind: LiveKind,
    /// Mouse paint: glow, uncover, reveal, boost or screen.
    pub look: String,
    /// Mouse paint and the trail: how long a key takes to fade back, ms (0 keeps the paint).
    pub fade_ms: u64,
    /// Mouse paint, look boost: the dimmed keys' brightness, %.
    pub dim: u32,
    /// Mouse paint and the trail: writes a second at most.
    pub mouse_fps: u32,
    /// Screen mirror: writes a second at most.
    pub screen_fps: u32,
    /// Screen mirror: how slowly the keys follow the screen, ms.
    pub smooth_ms: u64,
    /// Screen mirror: how much the screen's areas keep their own colours, 0-1.
    pub spread: f64,
    /// Screen mirror: how much deeper the colours are than the screen's averages, 1-4.
    pub saturation: f64,
    /// Screen mirror: read the screen on the graphics card (least processor, more memory) or through GDI.
    pub gpu: bool,
    /// The key-press effects over the base (any of laser, lightning, seismic).
    pub tap: Vec<TapKind>,
    /// Its colour: intended `#RRGGBB` (sent through the LED palette and balance), or `keys` for each key's own
    /// static colour. Over the screen mirror it's always the keys' own colours (the owner, 2026-11-25), so the
    /// beams show the static look through the screen's colours.
    pub tap_color: String,
    /// How fast a beam runs, keys a second.
    pub tap_speed: u32,
    /// Its writes a second at most while a beam runs.
    pub tap_fps: u32,
    /// The mouse interaction ([`MOUSE_MODES`]).
    pub mouse: String,
}

impl Default for LiveSettings {
    fn default() -> Self {
        LiveSettings {
            kind: LiveKind::Off,
            look: "glow".into(),
            fade_ms: mousepaint::DEFAULT_FADE_MS,
            dim: mousepaint::DEFAULT_DIM_PCT,
            mouse_fps: 10,
            screen_fps: screenmirror::DEFAULT_FPS,
            smooth_ms: screenmirror::DEFAULT_SMOOTH_MS,
            spread: screenmirror::DEFAULT_SPREAD,
            saturation: screenmirror::DEFAULT_SATURATION,
            gpu: true,
            tap: Vec::new(),
            tap_color: "#FFFFFF".into(),
            tap_speed: 30,
            tap_fps: 12,
            mouse: "off".into(),
        }
    }
}

/// A beam's speed, the range accepted, keys a second.
pub const TAP_SPEED_RANGE: std::ops::RangeInclusive<u32> = 5..=80;

impl LiveSettings {
    pub fn validate(&self) -> Result<(), String> {
        if mousepaint::look(&self.look).is_none() {
            return Err(format!("live: '{}' isn't a mouse paint look: use {}", self.look, LOOKS.join(", ")));
        }
        if !mousepaint::FADE_RANGE.contains(&self.fade_ms) {
            return Err(format!("live: a fade of {} ms is out of range: 0-10000", self.fade_ms));
        }
        if !(1..=100).contains(&self.dim) {
            return Err(format!("live: a dimmed base of {} % is out of range: 1-100", self.dim));
        }
        if !mousepaint::FPS_RANGE.contains(&self.mouse_fps) || !screenmirror::FPS_RANGE.contains(&self.screen_fps) || !(1..=30).contains(&self.tap_fps) {
            return Err("live: writes a second are out of range: 1-30".into());
        }
        if !screenmirror::SMOOTH_RANGE.contains(&self.smooth_ms) {
            return Err(format!("live: an ease of {} ms is out of range: 0-10000", self.smooth_ms));
        }
        if !(self.spread.is_finite() && (0.0..=1.0).contains(&self.spread)) {
            return Err(format!("live: spread {} is out of range: 0-1", self.spread));
        }
        if !(self.saturation.is_finite() && screenmirror::SATURATION_RANGE.contains(&self.saturation)) {
            return Err(format!("live: saturation {} is out of range: 1-4", self.saturation));
        }
        if self.tap_color != KEY_COLOURS {
            parse_rgb(&self.tap_color).map_err(|e| format!("live: the laser's colour: {e}"))?;
        }
        if !TAP_SPEED_RANGE.contains(&self.tap_speed) {
            return Err(format!("live: a laser at {} keys a second is out of range: 5-80", self.tap_speed));
        }
        if !MOUSE_MODES.contains(&self.mouse.as_str()) {
            return Err(format!("live: '{}' isn't a mouse interaction: use {}", self.mouse, MOUSE_MODES.join(", ")));
        }
        Ok(())
    }

    /// The screen mirror's options.
    pub fn ambient(&self) -> screenmirror::Ambient {
        screenmirror::Ambient { fps: self.screen_fps, smooth_ms: self.smooth_ms, spread: self.spread, saturation: self.saturation }
    }
}

// ---------------------------------------------------------------------------------------------------------
// The plan: what runs now.

/// What the keyboard shows under the interactions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Base {
    /// The base profile.
    Static,
    /// The base profile shown white, dark or dimmed (the mouse's uncover, reveal, boost).
    Look(ClockBase),
    /// The clock effect.
    Clock,
    /// The screen mirror effect.
    Screen,
}

/// What runs for these settings in this power state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub base: Base,
    pub tap: Taps,
    /// The mouse trail's brush, `None` with the mouse off.
    pub trail: Option<Brush>,
}

impl Plan {
    /// The display is off and the clock shows instead.
    pub fn clock_only() -> Plan {
        Plan { base: Base::Clock, tap: Taps::default(), trail: None }
    }
    pub fn needs_pointer(&self) -> bool {
        self.trail.is_some()
    }
    pub fn needs_keys(&self) -> bool {
        self.tap.any()
    }
}

/// What should run, or `None`: custom lighting, a lit keyboard, the (turned-down) reactive glow off, not the
/// keyboard's own effect (it replaces the per-key colours on the chip), and something to draw: the clock or
/// the screen mirror, a key-press effect, or a mouse interaction. With the display off and
/// `clockOnDisplayOff`, the clock alone (over any effect).
pub fn plan(s: &LightingSettings, p: &PowerState) -> Option<Plan> {
    if s.mode != Mode::Custom || s.reactive.on || lightctl::level(s, p) == 0 {
        return None;
    }
    if p.display_off && s.clock_on_display_off {
        return Some(Plan::clock_only());
    }
    let l = &s.live;
    let effect = match l.kind {
        LiveKind::Clock => Some(Base::Clock),
        LiveKind::Screen => Some(Base::Screen),
        LiveKind::Off | LiveKind::Mouse => None,
    };
    if effect.is_none() && s.keyboard.effect != lightctl::KeyEffect::Static {
        return None;
    }
    // The mouse: settings saved before the three layers kept it in `look` with kind `mouse`.
    let mode = if l.kind == LiveKind::Mouse { l.look.as_str() } else { l.mouse.as_str() };
    let (look, trail) = match mode {
        "off" => (None, None),
        "glow" => (None, Some(Brush::White)),
        "screen" => (None, Some(Brush::Screen)),
        m => {
            let (b, brush) = mousepaint::look(m)?;
            (Some(b), Some(brush))
        }
    };
    // Uncover, reveal and boost change how the base profile shows; over an effect they bring the static colours
    // through it where the pointer goes.
    let base = effect.unwrap_or(match look {
        Some(b) if b != ClockBase::Preset => Base::Look(b),
        _ => Base::Static,
    });
    let tap = Taps::of(&l.tap);
    if base == Base::Static && !tap.any() && trail.is_none() {
        return None;
    }
    Some(Plan { base, tap, trail })
}

/// Live lighting should run now.
pub fn wanted(s: &LightingSettings, p: &PowerState) -> bool {
    plan(s, p).is_some()
}

// ---------------------------------------------------------------------------------------------------------
// The layers.

/// A key press's effects: a laser out from the key pressed at `t0` along its row, lightning in to it, rings
/// out from it over the whole keyboard; whichever are on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beam {
    pub taps: Taps,
    pub row: u8,
    /// The pressed key's centre, key units (x across, y down).
    pub x: f64,
    pub y: f64,
    pub id: u8,
    pub t0: u64,
    /// The seismic rings' colour: the pressed key's colour in the static profile (sent).
    pub color: Rgb,
}

/// A beam head lights keys within this, key units (fading to nothing at the edge).
pub const BEAM_HALF: f64 = 1.0;
/// The pressed key itself flashes and fades with this time constant, ms.
pub const FLASH_MS: f64 = 220.0;
/// The row's ends, key units: lightning starts just outside them.
pub const ROW_ENDS: (f64, f64) = (-0.5, 16.5);
/// Lightning's bright tail behind each bolt, key units.
pub const BOLT_TAIL: f64 = 2.5;
/// Seismic: the ring's radius eases out toward this, key units (past the far corner from any key).
pub const RING_REACH: f64 = 18.0;
/// ...with this time constant at speed 30, ms: it erupts (half the keyboard in about 0.1 s) and slows (the
/// owner: "erupt faster, like a snap", 2026-11-25). Faster speeds shorten it.
pub const RING_TAU_MS: f64 = 140.0;
/// The ring lights keys within this of its radius, key units (wide, so a fast ring skips no key between
/// writes).
pub const RING_HALF: f64 = 1.5;
/// Inside the ring the keys glow up to this share (strongest just behind it).
pub const RING_WASH: f64 = 0.35;
/// The eruption fades with this time constant, ms.
pub const RING_FADE_MS: f64 = 400.0;

impl Beam {
    /// How long lightning takes to reach the key from the far end of its row, s.
    fn strike_s(&self, speed: u32) -> f64 {
        (self.x - ROW_ENDS.0).max(ROW_ENDS.1 - self.x) / speed.max(1) as f64
    }

    /// How much of the laser and lightning a key gets at `now`, 0..1 (the brightest of the parts that are on):
    /// - laser: the two heads `speed` keys a second out from the pressed key on its row, and the key's own
    ///   flash as it's pressed;
    /// - lightning: two bolts from the row's ends in to the key, arriving together, each with a fading tail,
    ///   and the key's flash when they strike.
    pub fn at(&self, id: u8, row: u8, x: f64, now: u64, speed: u32) -> f64 {
        let dt = now.saturating_sub(self.t0) as f64 / 1_000.0;
        let mut a: f64 = 0.0;
        if self.taps.laser {
            if id == self.id {
                a = (-(dt * 1_000.0) / FLASH_MS).exp();
            }
            if row == self.row {
                let run = dt * speed as f64;
                for head in [self.x - run, self.x + run] {
                    let d = (x - head).abs();
                    if d < BEAM_HALF {
                        a = a.max(1.0 - d / BEAM_HALF);
                    }
                }
            }
        }
        if self.taps.lightning {
            let t = self.strike_s(speed);
            if dt < t {
                if row == self.row {
                    let f = dt / t;
                    // Each bolt: its head, and a tail back toward the end it came from.
                    for (end, head) in [(ROW_ENDS.0, ROW_ENDS.0 + f * (self.x - ROW_ENDS.0)), (ROW_ENDS.1, ROW_ENDS.1 - f * (ROW_ENDS.1 - self.x))] {
                        let d = (x - head).abs();
                        if d < BEAM_HALF {
                            a = a.max(1.0 - d / BEAM_HALF);
                        }
                        let behind = if end < head { x < head && x > end } else { x > head && x < end };
                        if behind && d < BOLT_TAIL {
                            a = a.max(0.6 * (1.0 - d / BOLT_TAIL));
                        }
                    }
                }
            } else if id == self.id {
                a = a.max((-((dt - t) * 1_000.0) / FLASH_MS).exp());
            }
        }
        a
    }

    /// How much of the seismic eruption a key at (`x`, `y`) gets at `now`, 0..1: a ring bursting out from the
    /// pressed key, fast at first and slowing ([`RING_TAU_MS`], scaled by `speed`), a glow inside it, all
    /// fading ([`RING_FADE_MS`]); and the pressed key's own flash.
    pub fn ripple_at(&self, id: u8, x: f64, y: f64, now: u64, speed: u32) -> f64 {
        if !self.taps.seismic {
            return 0.0;
        }
        let ms = now.saturating_sub(self.t0) as f64;
        let mut a: f64 = if id == self.id { (-ms / FLASH_MS).exp() } else { 0.0 };
        let tau = RING_TAU_MS * 30.0 / speed.max(1) as f64;
        let r = RING_REACH * (1.0 - (-ms / tau).exp());
        let strength = (-ms / RING_FADE_MS).exp();
        let d = ((x - self.x).powi(2) + (y - self.y).powi(2)).sqrt();
        let off = (d - r).abs();
        let ring = if off < RING_HALF { 1.0 - off / RING_HALF } else { 0.0 };
        let wash = if d < r && r > 0.0 { RING_WASH * d / r } else { 0.0 };
        a = a.max(strength * ring.max(wash));
        a
    }

    /// Every part has finished: the laser's heads off the keyboard and its flash faded, the lightning struck
    /// and its flash faded, the rings faded out.
    pub fn done(&self, now: u64, speed: u32) -> bool {
        let dt = now.saturating_sub(self.t0) as f64 / 1_000.0;
        let laser = !self.taps.laser || (dt * speed as f64 > 17.0 + BEAM_HALF && dt * 1_000.0 > FLASH_MS * 6.0);
        let lightning = !self.taps.lightning || (dt - self.strike_s(speed)) * 1_000.0 > FLASH_MS * 6.0;
        let seismic = !self.taps.seismic || dt * 1_000.0 > RING_FADE_MS * 3.5;
        laser && lightning && seismic
    }
}

/// Each lit key's id, row and centre, key units (x across, y down).
fn key_spots() -> Vec<(u8, u8, f64, f64)> {
    x17::KEYS.iter().filter_map(|k| key_centre(k).map(|(x, y)| (k.id, k.row, x, y))).collect()
}

/// The effects for a press of LED `id` at `now`, the key showing `color` (`None` for a key with no light, or
/// none on).
pub fn beam_for(taps: Taps, id: u8, now: u64, color: Rgb) -> Option<Beam> {
    if !taps.any() {
        return None;
    }
    key_spots().into_iter().find(|(k, ..)| *k == id).map(|(id, row, x, y)| Beam { taps, row, x, y, id, t0: now, color })
}

/// `frame` with the effects drawn over it: the laser and lightning in `color(key)`, the rings in their
/// pressed key's colour (sent colours). Where several touch a key the strongest shows.
pub fn with_beams(frame: &mut [(u8, Rgb)], spots: &[(u8, u8, f64, f64)], beams: &[Beam], color: &dyn Fn(u8) -> Rgb, now: u64, speed: u32) {
    if beams.is_empty() {
        return;
    }
    for (id, c) in frame.iter_mut() {
        if let Some((_, row, x, y)) = spots.iter().find(|(k, ..)| k == id) {
            let mut best: (f64, Option<Rgb>) = (0.0, None);
            for b in beams {
                let line = b.at(*id, *row, *x, now, speed);
                if line > best.0 {
                    best = (line, Some(color(*id)));
                }
                let ring = b.ripple_at(*id, *x, *y, now, speed);
                if ring > best.0 {
                    best = (ring, Some(b.color));
                }
            }
            if let (a, Some(to)) = best {
                *c = mix(*c, to, a);
            }
        }
    }
}

/// The laser's and lightning's colour for each key: its own static colour over the screen mirror or with
/// `keys`, else the chosen colour.
pub fn beam_colours(s: &LightingSettings, base: Base, preset: &[(u8, Rgb)]) -> Result<Box<dyn Fn(u8) -> Rgb>, String> {
    if base == Base::Screen || s.live.tap_color == KEY_COLOURS {
        return Ok(key_colour(preset));
    }
    let c = s.led(parse_rgb(&s.live.tap_color)?);
    Ok(Box::new(move |_| c))
}

/// Each key's own static colour (sent), white for a key without one.
pub fn key_colour(preset: &[(u8, Rgb)]) -> Box<dyn Fn(u8) -> Rgb> {
    let p: Vec<(u8, Rgb)> = preset.to_vec();
    Box::new(move |id| p.iter().find(|(k, _)| *k == id).map_or(Rgb::new(255, 255, 255), |(_, c)| *c))
}

/// The base frame for a mouse paint look (sent colours): `boost` dims the static colours to `dim` %.
fn look_base(s: &LightingSettings, base: ClockBase, dim: u32) -> Vec<(u8, Rgb)> {
    match base {
        ClockBase::Dim => base_frame(s, ClockBase::Preset).into_iter().map(|(id, c)| (id, mix(Rgb::new(0, 0, 0), c, dim as f64 / 100.0))).collect(),
        b => base_frame(s, b),
    }
}

/// Where the loop reads the time, the pointer, the screen and key presses (LED ids since the last call).
pub struct Sources<'a> {
    pub time: &'a mut dyn FnMut() -> Hms,
    pub pointer: Option<&'a mut dyn PointerSource>,
    pub screen: Option<&'a mut dyn ScreenSource>,
    pub keys: Option<&'a mut dyn FnMut() -> Vec<u8>>,
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSummary {
    pub first: usize,
    pub frames: usize,
    pub reports: usize,
    pub keys: usize,
    pub ran_ms: u64,
}

/// How often the loop looks at its inputs, ms.
const POLL_MS: u64 = 20;
/// A still pointer's colour is read again this often, ms (screen-colour brush).
const RESAMPLE_MS: u64 = 250;

/// Runs `plan` on `io` until `stop(now)`, then writes the static colours back. AWCC is paused around each
/// write only.
#[allow(clippy::too_many_arguments)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, plan: Plan, clock: &mut dyn Clock, src: Sources, stop: &mut dyn FnMut(u64) -> bool) -> Result<LiveSummary, String> {
    s.validate()?;
    let l = &s.live;
    let Sources { time, mut pointer, mut screen, mut keys } = src;
    if plan.needs_pointer() && pointer.is_none() {
        return Err("the mouse pointer can't be read".into());
    }
    if plan.base == Base::Screen && screen.is_none() {
        return Err("the screen can't be read".into());
    }
    let t0 = clock.now_ms();
    let spots = key_spots();
    let mut beams: Vec<Beam> = Vec::new();
    let dark = base_frame(s, ClockBase::Off);
    let preset = base_frame(s, ClockBase::Preset);
    let white = s.led(Rgb::new(255, 255, 255));
    let tap_color = beam_colours(s, plan.base, &preset)?;
    let static_base = match plan.base {
        Base::Static => preset.clone(),
        Base::Look(b) => look_base(s, b, l.dim),
        Base::Clock | Base::Screen => dark.clone(),
    };
    // The screen mirror: exact colours easing toward the screen, and the snapshot the keyboard shows (taken
    // whole, when any key moved more than the mirror's threshold).
    let amb = l.ambient();
    let mut mirror: Option<(Vec<(u8, [f64; 3])>, Vec<(u8, Rgb)>)> = None;
    if plan.base == Base::Screen {
        let g = screen.as_mut().and_then(|sc| sc.grab()).ok_or("the screen can't be read")?;
        let now_c = screenmirror::aim(s, &g, &amb);
        let shown_c = now_c.iter().map(|(id, c)| (*id, screenmirror::to_rgb(*c))).collect();
        mirror = Some((now_c, shown_c));
    }
    let screen_size = pointer.as_mut().map(|ptr| ptr.screen());
    let mut paint: Vec<(u8, Rgb, u64)> = Vec::new();
    let mut last: Option<((i32, i32), (f64, f64))> = None;
    let mut last_sample = 0u64;

    let picture = |now: u64, hms: Hms, mirror: &Option<(Vec<(u8, [f64; 3])>, Vec<(u8, Rgb)>)>, paint: &mut Vec<(u8, Rgb, u64)>, beams: &[Beam]| -> Vec<(u8, Rgb)> {
        let mut f = match plan.base {
            Base::Clock => clock::draw(s, &dark, hms),
            Base::Screen => {
                let mut f = dark.clone();
                if let Some((_, shown_c)) = mirror {
                    for (id, c) in f.iter_mut() {
                        if let Some((_, m)) = shown_c.iter().find(|(k, _)| k == id) {
                            *c = *m;
                        }
                    }
                }
                f
            }
            _ => static_base.clone(),
        };
        with_beams(&mut f, &spots, beams, &*tap_color, now, l.tap_speed);
        if plan.trail.is_some() {
            f = mousepaint::frame(&f, paint, now, l.fade_ms);
        }
        f
    };

    let mut shown = picture(t0, (time)(), &mirror, &mut paint, &beams);
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
            return Err(format!("live lighting couldn't start: {e}"));
        }
    };
    let mut sum = LiveSummary { first, ..Default::default() };
    // Writes at most as often as the fastest layer needs (the clock alone: whenever the time changes).
    let fps = [
        plan.needs_keys().then_some(l.tap_fps),
        plan.trail.is_some().then_some(l.mouse_fps),
        (plan.base == Base::Screen).then_some(l.screen_fps),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(20);
    let interval = 1_000 / fps.max(1) as u64;
    let grab_every = 1_000 / l.screen_fps.max(1) as u64;
    let (mut next_write, mut next_grab, mut last_grab) = (t0, t0 + grab_every, t0);
    let mut failed = None;
    loop {
        let now = clock.now_ms();
        if clock.stopped() || stop(now) {
            break;
        }
        // The screen mirror: read, ease, and take a new snapshot when a key moved enough.
        if let (Some((now_c, shown_c)), Some(sc)) = (mirror.as_mut(), screen.as_mut()) {
            if now >= next_grab {
                next_grab = now + grab_every;
                if let Some(g) = sc.grab() {
                    let k = if amb.smooth_ms == 0 { 1.0 } else { 1.0 - (-((now - last_grab) as f64) / amb.smooth_ms as f64).exp() };
                    last_grab = now;
                    for ((_, c), (_, t)) in now_c.iter_mut().zip(screenmirror::aim(s, &g, &amb)) {
                        for ch in 0..3 {
                            c[ch] += (t[ch] - c[ch]) * k;
                        }
                    }
                    let next: Vec<(u8, Rgb)> = now_c.iter().map(|(id, c)| (*id, screenmirror::to_rgb(*c))).collect();
                    if next.iter().zip(shown_c.iter()).any(|((_, a), (_, b))| diff(*a, *b) > screenmirror::THRESHOLD) {
                        *shown_c = next;
                    }
                }
            }
        }
        // The trail: the keys the pointer passed, in the brush's colour.
        if let (Some(brush), Some(ptr), Some(size)) = (plan.trail, pointer.as_mut(), screen_size) {
            if let Some(pos) = ptr.position() {
                let here = size.share(pos.0, pos.1);
                let moved = last.is_none_or(|(q, _)| q != pos);
                let sampled = if brush == Brush::Screen && (moved || now >= last_sample + RESAMPLE_MS) {
                    last_sample = now;
                    ptr.color_at(pos.0, pos.1).map(|c| s.led(c))
                } else {
                    None
                };
                if moved || sampled.is_some() {
                    let path = match last {
                        Some((_, from)) if moved => mousepaint::keys_along(from, here),
                        _ => mousepaint::key_at(here.0, here.1).into_iter().collect(),
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
                        match paint.iter_mut().find(|(k, _, _)| *k == id) {
                            Some(slot) => *slot = (id, c, now),
                            None => paint.push((id, c, now)),
                        }
                    }
                }
                // The key under the pointer stays fully lit until the pointer leaves it.
                if let Some(slot) = mousepaint::key_at(here.0, here.1).and_then(|id| paint.iter_mut().find(|(k, _, _)| *k == id)) {
                    slot.2 = now;
                }
                last = Some((pos, here));
            }
        }
        // Key presses: a beam each; finished beams go.
        if let Some(k) = keys.as_mut() {
            for id in (k)() {
                if let Some(b) = beam_for(plan.tap, id, now, key_colour(&preset)(id)) {
                    beams.push(b);
                }
            }
        }
        beams.retain(|b| !b.done(now, l.tap_speed));
        if now >= next_write {
            let next = picture(now, (time)(), &mirror, &mut paint, &beams);
            let changed = delta(&shown, &next, 0);
            if !changed.is_empty() {
                let keys: Vec<Key> = changed.iter().map(|(id, c)| Key::new(*id, *c)).collect();
                match frame_steps(FrameKind::Full, &keys).and_then(|st| send(dev.as_mut(), pauser, &st)) {
                    Ok(n) => {
                        sum.frames += 1;
                        sum.reports += n;
                        sum.keys += changed.len();
                        shown = next;
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
        (None, Ok(_)) => Ok(sum),
        (Some(e), Ok(a)) => Err(format!("live lighting stopped: {e}. The static colours were written back: {}", a.text())),
        (Some(e), Err(r)) => Err(format!("live lighting stopped: {e}. Writing the static colours back failed too: {r}")),
        (None, Err(r)) => Err(format!("live lighting stopped, but writing the static colours back failed: {r}")),
    }
}

/// [`run`] with the real clock, pointer and screen (Windows). Returns what it did, as text.
#[allow(unused_variables)]
/// `keys` gives the key presses (LED ids) since the last call, for the key-press effect: the app's listener,
/// or the command line's own.
#[allow(clippy::too_many_arguments)]
pub fn run_windows(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, clock: &mut dyn Clock, mut keys: Option<&mut dyn FnMut() -> Vec<u8>>, stop: &mut dyn FnMut(u64) -> bool) -> Result<String, String> {
    let plan = plan(s, p).ok_or("nothing to run")?;
    #[cfg(windows)]
    {
        let mut ptr = plan.needs_pointer().then(mousepaint::win::WinPointer::new);
        let mut scr = if plan.base == Base::Screen { Some(screenmirror::open(s.live.gpu)?) } else { None };
        let mut time = clock::local_now;
        let mut presses = || keys.as_mut().map(|k| k()).unwrap_or_default();
        let src = Sources {
            time: &mut time,
            pointer: ptr.as_mut().map(|x| x as &mut dyn PointerSource),
            screen: match scr.as_mut() {
                Some(b) => Some(b.as_mut()),
                None => None,
            },
            keys: plan.needs_keys().then_some(&mut presses as &mut dyn FnMut() -> Vec<u8>),
        };
        let sum = run(io, s, p, prev, pauser, plan, clock, src, stop)?;
        Ok(format!("Live lighting ran {:.0} s ({} writes); the static colours are back.", sum.ran_ms as f64 / 1_000.0, sum.frames))
    }
    #[cfg(not(windows))]
    {
        let _ = (plan, keys.take());
        Err(lightctl::WINDOWS_ONLY.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightctl::FakeIo;
    use crate::mousepaint::Screen;
    use alienfx::device::FakePauser;

    fn custom() -> LightingSettings {
        LightingSettings { mode: Mode::Custom, ..Default::default() }
    }

    #[test]
    fn defaults_are_the_owners_picks_and_valid() {
        let l = LiveSettings::default();
        assert_eq!((l.kind, l.look.as_str(), l.dim, l.spread, l.saturation, l.gpu), (LiveKind::Off, "glow", 70, 1.0, 2.0, true));
        assert_eq!((l.tap.is_empty(), l.tap_speed, l.mouse.as_str()), (true, 30, "off"));
        l.validate().unwrap();
        // Older settings files have no `live` (or only some of it): the rest is the defaults.
        let s: LightingSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.live, LiveSettings::default());
        assert!(!s.clock_on_display_off);
        let s: LightingSettings = serde_json::from_str(r#"{"live":{"kind":"screen","spread":0.5,"tap":["laser","seismic"]}}"#).unwrap();
        assert_eq!((s.live.kind, s.live.spread, s.live.tap.clone(), s.live.look.as_str()), (LiveKind::Screen, 0.5, vec![TapKind::Laser, TapKind::Seismic], "glow"));
    }

    #[test]
    fn bad_options_are_refused() {
        let bad = |f: &dyn Fn(&mut LiveSettings)| {
            let mut l = LiveSettings::default();
            f(&mut l);
            l.validate().is_err()
        };
        assert!(bad(&|l| l.look = "sparkle".into()));
        assert!(bad(&|l| l.fade_ms = 20_000));
        assert!(bad(&|l| l.dim = 0));
        assert!(bad(&|l| l.mouse_fps = 0));
        assert!(bad(&|l| l.screen_fps = 31));
        assert!(bad(&|l| l.tap_fps = 0));
        assert!(bad(&|l| l.spread = 1.5));
        assert!(bad(&|l| l.saturation = 0.5));
        assert!(bad(&|l| l.smooth_ms = 60_000));
        assert!(bad(&|l| l.tap_color = "laser red".into()));
        assert!(!bad(&|l| l.tap_color = KEY_COLOURS.into()), "each key's own colour is allowed");
        assert!(bad(&|l| l.tap_speed = 200));
        assert!(bad(&|l| l.mouse = "glitter".into()));
        let mut s = LightingSettings::default();
        s.live.look = "sparkle".into();
        assert!(s.validate().is_err(), "the lighting settings check the live options too");
    }

    #[test]
    fn the_plan_follows_the_three_layers_and_the_display() {
        let p = PowerState::default();
        let mut s = custom();
        assert_eq!(plan(&s, &p), None, "only the base profile: nothing to run");
        s.live.tap = vec![TapKind::Laser];
        let laser = Taps { laser: true, ..Default::default() };
        assert_eq!(plan(&s, &p), Some(Plan { base: Base::Static, tap: laser, trail: None }), "a key-press laser over the profile");
        s.live.mouse = "glow".into();
        assert_eq!(plan(&s, &p).unwrap().trail, Some(Brush::White), "and a white trail");
        s.live.kind = LiveKind::Screen;
        s.live.mouse = "screen".into();
        assert_eq!(plan(&s, &p), Some(Plan { base: Base::Screen, tap: laser, trail: Some(Brush::Screen) }), "over the screen mirror");
        // Uncover / reveal / boost change how the profile shows; over an effect they bring the colours through.
        s.live.kind = LiveKind::Off;
        s.live.mouse = "reveal".into();
        assert_eq!(plan(&s, &p).unwrap().base, Base::Look(ClockBase::Off));
        assert_eq!(plan(&s, &p).unwrap().trail, Some(Brush::Preset));
        s.live.kind = LiveKind::Clock;
        assert_eq!(plan(&s, &p).unwrap().base, Base::Clock, "reveal through the clock");
        // The keyboard's own effect runs on the chip: nothing of this with it (but the clock or mirror replace it).
        s.keyboard.effect = lightctl::KeyEffect::Breathing;
        assert_eq!(plan(&s, &p).unwrap().base, Base::Clock);
        s.live.kind = LiveKind::Off;
        assert_eq!(plan(&s, &p), None, "breathing on the chip: interactions rest");
        s.keyboard.effect = lightctl::KeyEffect::Static;
        // Settings saved before the three layers: kind mouse with its look.
        let old: LightingSettings = serde_json::from_str(r#"{"mode":"custom","live":{"kind":"mouse","look":"boost"}}"#).unwrap();
        assert_eq!(plan(&old, &p).map(|pl| (pl.base, pl.trail)), Some((Base::Look(ClockBase::Dim), Some(Brush::Preset))));
        // The display off: dark by default; the clock alone with clockOnDisplayOff.
        let off = PowerState { display_off: true, ..Default::default() };
        assert_eq!(plan(&s, &off), None, "dark");
        s.clock_on_display_off = true;
        assert_eq!(plan(&s, &off), Some(Plan::clock_only()));
        s.live = LiveSettings::default();
        assert_eq!(plan(&s, &off), Some(Plan::clock_only()), "even with nothing live chosen");
        assert_eq!(plan(&s, &p), None, "and nothing once it's back on");
        assert!(lightctl::level(&s, &off) > 0, "the keyboard stays lit for the clock");
        // Never in AWCC mode, with the keyboard off, or with the reactive glow.
        s.mode = Mode::Awcc;
        assert_eq!(plan(&s, &off), None);
        s.mode = Mode::Custom;
        s.keyboard.on = false;
        assert_eq!(plan(&s, &off), None);
        s.keyboard.on = true;
        s.reactive.on = true;
        assert_eq!(plan(&s, &off), None);
    }

    #[test]
    fn a_press_sends_a_beam_both_ways_along_its_row() {
        let g = beam_for(Taps { laser: true, ..Default::default() }, x17::by_name("G").unwrap().id, 0, Rgb::new(255, 255, 255)).unwrap();
        let spot = |n: &str| key_spots().into_iter().find(|(id, ..)| *id == x17::by_name(n).unwrap().id).unwrap();
        let at = |n: &str, ms: u64| {
            let (id, row, x, _) = spot(n);
            g.at(id, row, x, ms, 30)
        };
        assert!(at("G", 0) > 0.9, "the pressed key flashes");
        assert_eq!(at("T", 0), 0.0, "other rows stay");
        // At 30 keys a second, 100 ms later the heads are 3 keys out: S and K.
        assert!(at("S", 100) > 0.9 && at("K", 100) > 0.9, "{} {}", at("S", 100), at("K", 100));
        assert_eq!(at("A", 100), 0.0, "not there yet");
        assert!(at("A", 135) > 0.5, "then further out");
        assert_eq!(at("D", 300), 0.0, "and gone behind");
        assert!(!g.done(300, 30) && g.done(1_400, 30), "done once off the keyboard and the flash has faded");
        assert_eq!(beam_for(g.taps, 200, 0, g.color), None, "no beam for a key without a light");
        assert_eq!(beam_for(Taps::default(), g.id, 0, g.color), None);
    }

    #[test]
    fn over_the_screen_mirror_beams_take_the_keys_own_colours() {
        let mut s = custom();
        let g = x17::by_name("G").unwrap().id;
        s.keyboard.keys.insert(g as u32, "#00E5FF".into());
        s.live.tap_color = "#FF0000".into();
        let preset = base_frame(&s, ClockBase::Preset);
        let own = preset.iter().find(|(k, _)| *k == g).unwrap().1;
        assert_eq!(beam_colours(&s, Base::Screen, &preset).unwrap()(g), own, "screen mirror: the key's own colour");
        assert_eq!(beam_colours(&s, Base::Static, &preset).unwrap()(g), s.led(Rgb::new(255, 0, 0)), "elsewhere: the chosen colour");
        s.live.tap_color = KEY_COLOURS.into();
        assert_eq!(beam_colours(&s, Base::Clock, &preset).unwrap()(g), own, "or the keys' own colours when chosen");
    }

    #[test]
    fn lightning_comes_in_from_both_ends_and_strikes_the_key() {
        let g = beam_for(Taps { lightning: true, ..Default::default() }, x17::by_name("G").unwrap().id, 0, Rgb::new(255, 255, 255)).unwrap();
        let spot = |n: &str| key_spots().into_iter().find(|(id, ..)| *id == x17::by_name(n).unwrap().id).unwrap();
        let at = |n: &str, ms: u64| {
            let (id, row, x, _) = spot(n);
            g.at(id, row, x, ms, 30)
        };
        // G sits at 6.25 keys; the far end is 10.25 keys away: at 30 keys a second it strikes at ~342 ms.
        assert!(at("A", 100) > 0.0, "the left bolt is near A early on");
        assert!(at("Enter", 60) > 0.0, "the right bolt comes in from the right end (Enter first)");
        assert_eq!(at("G", 100), 0.0, "the key waits for the strike");
        assert!(at("G", 360) > 0.8, "then flashes");
        assert_eq!(at("T", 100), 0.0, "other rows stay");
        assert!(!g.done(400, 30) && g.done(1_800, 30));
        // Both: the laser's flash at once and the strike later.
        let both = beam_for(Taps { laser: true, lightning: true, seismic: false }, g.id, 0, g.color).unwrap();
        let (id, row, x, _) = spot("G");
        assert!(both.at(id, row, x, 0, 30) > 0.9 && both.at(id, row, x, 360, 30) > 0.8);
    }

    #[test]
    fn seismic_rings_spread_over_the_whole_keyboard_in_the_keys_colour() {
        let g = beam_for(Taps { seismic: true, ..Default::default() }, x17::by_name("G").unwrap().id, 0, Rgb::new(0, 176, 224)).unwrap();
        let spot = |n: &str| key_spots().into_iter().find(|(id, ..)| *id == x17::by_name(n).unwrap().id).unwrap();
        let at = |n: &str, ms: u64| {
            let (id, _, x, y) = spot(n);
            g.ripple_at(id, x, y, ms, 30)
        };
        assert!(at("G", 0) > 0.9, "the epicentre flashes");
        // A snap: the ring is at D and J (2 keys out) by 20 ms and at the far corner (Delete, ~10 keys away) by 100 ms.
        assert!(at("D", 20) > 0.3 && at("J", 20) > 0.3, "{} {}", at("D", 20), at("J", 20));
        assert!(at("Delete", 100) > 0.3, "the corner at 0.1 s: {}", at("Delete", 100));
        assert!(at("Delete", 30) < 0.1, "but not at once");
        assert!(at("Delete", 600) < at("Delete", 100), "then it fades");
        assert!(!g.done(300, 30) && g.done(1_500, 30));
        assert_eq!(g.at(g.id, g.row, g.x, 0, 30), 0.0, "no laser part");
        // The rings erupt in the beam's colour (the key's static colour); the laser keeps the chosen one.
        let mut s = custom();
        s.live.tap_color = "#FF0000".into();
        let preset = base_frame(&s, ClockBase::Preset);
        let mut f = base_frame(&s, ClockBase::Off);
        with_beams(&mut f, &key_spots(), &[g], &*beam_colours(&s, Base::Static, &preset).unwrap(), 0, 30);
        assert_eq!(f.iter().find(|(k, _)| *k == g.id).unwrap().1, g.color, "at the epicentre, its colour");
    }

    #[test]
    fn a_press_erupts_in_its_keys_static_colour() {
        let mut s = custom();
        let g = x17::by_name("G").unwrap().id;
        s.keyboard.keys.insert(g as u32, "#00E5FF".into());
        let preset = base_frame(&s, ClockBase::Preset);
        let own = preset.iter().find(|(k, _)| *k == g).unwrap().1;
        assert_eq!(key_colour(&preset)(g), own, "the static profile's colour for that key, whatever the base shows");
    }

    struct Sim(u64, FakePauser);
    impl Clock for Sim {
        fn now_ms(&mut self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            assert_eq!(self.1.paused(), self.1.resumed(), "AWCC resumed before every wait");
            self.0 += ms;
        }
    }

    /// A pointer sweeping the home row over 2 s, then still.
    struct Sweep(u64);
    impl PointerSource for Sweep {
        fn screen(&mut self) -> Screen {
            Screen { x: 0, y: 0, w: 1600, h: 600 }
        }
        fn position(&mut self) -> Option<(i32, i32)> {
            self.0 += 1;
            Some((((self.0 * POLL_MS).min(2_000) as f64 / 2_000.0 * 1599.0) as i32, 350))
        }
        fn color_at(&mut self, _x: i32, _y: i32) -> Option<Rgb> {
            Some(Rgb::new(255, 0, 0))
        }
    }

    #[test]
    fn key_lasers_with_a_trail_run_over_the_static_colours() {
        let mut s = custom();
        s.live.tap = vec![TapKind::Laser];
        s.live.mouse = "glow".into();
        let p = PowerState::default();
        let plan = plan(&s, &p).unwrap();
        let pz = FakePauser::default();
        let mut ptr = Sweep(0);
        let mut time = || Hms { h: 9, m: 0, s: 0 };
        // A press of G every second for 3 s, then nothing.
        let (g, mut polls) = (x17::by_name("G").unwrap().id, 0u64);
        let mut presses = || {
            polls += 1;
            if polls % 50 == 1 && polls < 150 { vec![g] } else { vec![] }
        };
        let src = Sources { time: &mut time, pointer: Some(&mut ptr), screen: None, keys: Some(&mut presses) };
        let sum = run(&mut FakeIo::default(), &s, &p, Shown::of(&s, &p), Some(&pz), plan, &mut Sim(0, pz.clone()), src, &mut |now| now >= 8_000).unwrap();
        assert!(sum.frames > 10 && sum.frames <= 12 * 8, "writes while beams and the trail move, at most 12 a second: {}", sum.frames);
        assert!(sum.keys / sum.frames.max(1) <= 30, "a few keys a write: {} keys in {} writes", sum.keys, sum.frames);
        assert_eq!(pz.paused(), pz.resumed());
    }

    #[test]
    fn the_clock_alone_writes_only_when_the_time_changes() {
        let s = LightingSettings { clock_on_display_off: true, ..custom() };
        let off = PowerState { display_off: true, ..Default::default() };
        let plan = plan(&s, &off).unwrap();
        let mut t = 0u64;
        let mut time = || {
            t += POLL_MS;
            Hms::from_secs(17 * 3_600 + 42 * 60 + 5 + t / 1_000)
        };
        let src = Sources { time: &mut time, pointer: None, screen: None, keys: None };
        let pz = FakePauser::default();
        let sum = run(&mut FakeIo::default(), &s, &off, Shown::of(&s, &off), None, plan, &mut Sim(0, pz), src, &mut |now| now >= 10_000).unwrap();
        // 5:42:05 to :15: still to :09, then the seconds view (:10 to :14) and back at :15.
        assert!(sum.frames <= 8, "a handful of writes: {}", sum.frames);
    }
}
