//! Reactive glow (docs/lighting-control.md, "Reactive glow"): each key press sends warmth into that key. The
//! key flares toward a warm colour, a little warmth spreads to its neighbours, and it fades back to the key's
//! own colour in about a second. A warm centre also drifts after where the person has been typing, and keys
//! near it lean slightly warmer.
//!
//! - [`ReactiveState`]: per-key heat, the warm centre and its weight. Pure: [`ReactiveState::press`],
//!   [`ReactiveState::tick`] and [`ReactiveState::frame`] take times in ms on any steady clock.
//! - [`delta`]: only the keys whose sent colour changed by more than a few units.
//! - [`Runner`]: drives a [`LightIo`]: AWCC paused for the whole run, the full frame first, then delta frames
//!   capped at `fps`, the static settings written back on stop or after a failed write. [`run`] is its loop
//!   with the clock and the key source given (tests use a fake clock and `FakeIo`).
//!
//! Colours are blended in **sent (LED) space**: between a key's sent colour (`LightingSettings::led`) and the
//! sent warm colour, so the owner's colours matched by eye stay what the keyboard shows at rest. The
//! awareness keys ([`AWARENESS`]) never change.
//!
//! Privacy: a press is an LED id and a time; it only moves heat values in memory. Nothing records which keys
//! were pressed.

use crate::lightctl::{self, compose, parse_rgb, rgb_hex, Applied, KeyEffect, LightIo, LightingSettings, Mode, PauseLease, PowerState, Shown};
use alienfx::device::{check_steps, run_steps, LightDevice, Pauser};
use alienfx::v5::{self, GlobalEffect, Key, V5Step};
use alienfx::{x17, Api, Rgb};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------------------
// Settings.

/// The warm colour's default: the owner's peach (intended; sent `#FF5030` on these LEDs).
pub const DEFAULT_WARM: &str = "#FF9480";
/// The hint at the peak of a press: the owner's coral (intended; sent `#F83430`).
pub const DEFAULT_PEAK: &str = "#FF6F61";
/// The owner's matches for these two colours on the x17 R2's LEDs (intended -> sent, 2026-11-24), used when
/// `ledPalette` has no entry for them, so the defaults show as matched with any settings file.
pub const MATCHED: [(&str, &str); 2] = [("#FF9480", "#FF5030"), ("#FF6F61", "#F83430")];

/// Frames per second, the range the settings accept.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 5..=30;
/// The fade's time constant, ms, the range the settings accept.
pub const DECAY_RANGE: std::ops::RangeInclusive<u32> = 200..=3_000;
/// Most warmth a press adds to a neighbour (`spread`).
pub const MAX_SPREAD: f64 = 1.0;
/// Most lean toward warm at the warm centre (`drift`).
pub const MAX_DRIFT: f64 = 0.5;

/// How each glow frame is sent to the keyboard.
/// - `full`: reset, status read, the changed colours, loop, update (the proven per-key sequence). On the
///   owner's keyboard at 18 fps this blinked and slowed typing (2026-11-24): the reset and status read make the
///   keyboard's chip restart its colour set every frame.
/// - `light`: only the changed colours, loop, update: no reset, no status read. On the owner's keyboard: no
///   blinking and typing normal, but the presses didn't visibly glow (the keyboard seems to need the reset).
/// - `quick`: reset, the changed colours, loop, update: the status read (a request the keyboard's chip has to
///   answer every frame) left out.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    #[default]
    Full,
    Light,
    Quick,
}

/// The reactive glow's parameters (`lighting.reactive` in settings.json).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ReactiveSettings {
    /// Run the glow while the lighting is custom and static (the app). Default off.
    pub on: bool,
    /// Frames per second at most, 10..=30 (default 18).
    pub fps: u32,
    /// The fade's time constant, ms (default 700: a key is back to its own colour in about a second).
    pub decay_ms: u32,
    /// Warmth a press adds to its direct neighbours, 0..=1 (default 0.35), falling off to nothing at 1.5 key
    /// units.
    pub spread: f64,
    /// How far keys at the warm centre lean toward warm, 0..=0.5 (default 0.12).
    pub drift: f64,
    /// The warm colour, intended `#RRGGBB` (default the owner's peach).
    pub warm: String,
    /// The hint of colour at a press's peak, intended `#RRGGBB` (default the owner's coral).
    pub peak: String,
    /// How each frame is sent ([`FrameKind`]).
    pub frame: FrameKind,
}

impl Default for ReactiveSettings {
    fn default() -> Self {
        ReactiveSettings { on: false, fps: 18, decay_ms: 700, spread: 0.35, drift: 0.12, warm: DEFAULT_WARM.into(), peak: DEFAULT_PEAK.into(), frame: FrameKind::Full }
    }
}

impl ReactiveSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !FPS_RANGE.contains(&self.fps) {
            return Err(format!("reactive glow: {} frames per second is out of range: 5–30", self.fps));
        }
        if !DECAY_RANGE.contains(&self.decay_ms) {
            return Err(format!("reactive glow: a fade of {} ms is out of range: 200–3000 ms", self.decay_ms));
        }
        if !(self.spread.is_finite() && (0.0..=MAX_SPREAD).contains(&self.spread)) {
            return Err(format!("reactive glow: spread {} is out of range: 0–1", self.spread));
        }
        if !(self.drift.is_finite() && (0.0..=MAX_DRIFT).contains(&self.drift)) {
            return Err(format!("reactive glow: drift {} is out of range: 0–0.5", self.drift));
        }
        parse_rgb(&self.warm).map_err(|e| format!("reactive glow warm colour: {e}"))?;
        parse_rgb(&self.peak).map_err(|e| format!("reactive glow peak colour: {e}"))?;
        Ok(())
    }

    /// Time between frames at `fps`, ms (rounded up: 18 fps is a frame every 56 ms).
    pub fn interval_ms(&self) -> u64 {
        let fps = self.fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end()) as u64;
        1_000_u64.div_ceil(fps)
    }
}

/// The reports for one glow frame of `keys`, by [`FrameKind`].
pub fn frame_steps(kind: FrameKind, keys: &[Key]) -> Result<Vec<V5Step>, String> {
    match kind {
        FrameKind::Full => v5::apply_keys(keys).map_err(|e| e.to_string()),
        FrameKind::Light | FrameKind::Quick => {
            let mut out: Vec<V5Step> = if kind == FrameKind::Quick { vec![alienfx::Step::Write(v5::reset_report())] } else { Vec::new() };
            out.extend(v5::set_keys(keys).map_err(|e| e.to_string())?.into_iter().map(alienfx::Step::Write));
            out.push(alienfx::Step::Write(v5::update()));
            Ok(out)
        }
    }
}

/// The value to send for intended colour `c`: the owner's `ledPalette` entry, else [`MATCHED`], else the LED
/// balance (`LightingSettings::led`).
pub fn sent(s: &LightingSettings, c: Rgb) -> Rgb {
    let hex = rgb_hex(c);
    if s.led_palette.keys().any(|k| k.eq_ignore_ascii_case(&hex)) {
        return s.led(c);
    }
    MATCHED.iter().find(|(i, _)| i.eq_ignore_ascii_case(&hex)).and_then(|(_, o)| parse_rgb(o).ok()).unwrap_or_else(|| s.led(c))
}

// ---------------------------------------------------------------------------------------------------------
// The model.

/// Keys that keep their colour whatever happens: the red risk keys (Esc, Backspace, Delete, Mic mute) and Tab
/// (off in the owner's look). They still pass warmth on to their neighbours when pressed.
pub const AWARENESS: [u8; 5] = [0, 35, 15, 19, 40];
/// A key's heat counts this much toward its blend (the rest of the way is the drift's).
pub const HEAT_GAIN: f64 = 0.85;
/// Warmth spreads to keys whose centres are closer than this, key units.
pub const SPREAD_RADIUS: f64 = 1.5;
/// The warm centre follows presses with an exponential moving average over this long, ms.
pub const CENTRE_TAU_MS: f64 = 8_000.0;
/// The centre's weight fades with this time constant, ms (typing tops it up).
pub const WEIGHT_TAU_MS: f64 = 10_000.0;
/// Each press moves the weight this share of the way to 1 (steady typing settles around 0.8).
pub const WEIGHT_GAIN: f64 = 0.08;
/// The drift reaches this far from the centre, key units.
pub const DRIFT_RADIUS: f64 = 6.0;
/// Above this heat the warm colour leans toward the peak colour...
pub const PEAK_FROM: f64 = 0.8;
/// ...by up to this share at heat 1.
pub const PEAK_MIX: f64 = 0.4;
/// A key is sent again when a channel moved more than this many units from what was last sent.
pub const DELTA_THRESHOLD: u8 = 3;
/// With no key warm, only the drift is fading: keys are sent again only past this many units...
pub const DRIFT_THRESHOLD: u8 = 6;
/// ...and at most every this many ms, so the slow fade of the drift costs a few reports a second.
pub const DRIFT_INTERVAL_MS: u64 = 500;
/// At most two colorSet reports a frame; the rest catch up next frame (largest changes first).
pub const MAX_KEYS_PER_FRAME: usize = 2 * v5::KEYS_PER_REPORT;
/// Heat under this is zero (under 0.25 of a unit of colour): idle is exactly the static colours.
const SNAP: f64 = 1e-3;
/// The centre's weight under this is zero (the drift is then under a unit of colour at most).
const WEIGHT_SNAP: f64 = 1e-2;

/// A key's centre on the map, key units (x across, y down), `None` for an id with no key here.
pub fn key_centre(k: &x17::KeyDef) -> Option<(f64, f64)> {
    k.present.then(|| ((k.col as f64 + k.width as f64 / 2.0) / 4.0, k.row as f64 + 0.5))
}

fn index(id: u8) -> Option<usize> {
    x17::KEYS.iter().position(|k| k.id == id)
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// `a` to `b` by `f` (0..=1) per channel, rounded: `f` 0 is exactly `a`.
pub fn mix(a: Rgb, b: Rgb, f: f64) -> Rgb {
    let f = f.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * f).round().clamp(0.0, 255.0) as u8;
    Rgb::new(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b))
}

/// The largest channel difference.
pub fn diff(a: Rgb, b: Rgb) -> u8 {
    a.r.abs_diff(b.r).max(a.g.abs_diff(b.g)).max(a.b.abs_diff(b.b))
}

/// Per-key heat (0..=1, in the order of `x17::KEYS`), the warm centre with its weight, and the time.
#[derive(Debug, Clone, PartialEq)]
pub struct ReactiveState {
    heat: Vec<f64>,
    pos: Vec<Option<(f64, f64)>>,
    centre: (f64, f64),
    /// The centre's moving average: decayed sums of x, y and count.
    sums: (f64, f64, f64),
    weight: f64,
    t: u64,
    last_press: Option<u64>,
    decay_ms: f64,
    spread: f64,
}

impl ReactiveState {
    /// Everything cold at time `t` (ms); the centre starts in the middle of the keyboard with no weight.
    pub fn new(r: &ReactiveSettings, t: u64) -> Self {
        ReactiveState {
            heat: vec![0.0; x17::KEYS.len()],
            pos: x17::KEYS.iter().map(key_centre).collect(),
            centre: (x17::ROW_WIDTH as f64 / 8.0, x17::ROWS as f64 / 2.0),
            sums: (0.0, 0.0, 0.0),
            weight: 0.0,
            t,
            last_press: None,
            decay_ms: r.decay_ms.max(1) as f64,
            spread: r.spread.clamp(0.0, MAX_SPREAD),
        }
    }

    /// New parameters (the fade and the spread), the heat kept.
    pub fn set_params(&mut self, r: &ReactiveSettings) {
        self.decay_ms = r.decay_ms.max(1) as f64;
        self.spread = r.spread.clamp(0.0, MAX_SPREAD);
    }

    /// The fade up to `t`: heat decays with the `decay_ms` time constant, the centre's weight with
    /// [`WEIGHT_TAU_MS`]. Times before the state's own are ignored.
    pub fn tick(&mut self, t: u64) {
        if t <= self.t {
            return;
        }
        let dt = (t - self.t) as f64;
        let k = (-dt / self.decay_ms).exp();
        for h in &mut self.heat {
            *h *= k;
            if *h < SNAP {
                *h = 0.0;
            }
        }
        self.weight *= (-dt / WEIGHT_TAU_MS).exp();
        if self.weight < WEIGHT_SNAP {
            self.weight = 0.0;
        }
        self.t = t;
    }

    /// A key-down of LED `id` at `t`: that key to full heat, up to `spread` more on each neighbour (all of it
    /// at one key unit, nothing from [`SPREAD_RADIUS`]), and the warm centre moved toward it. Ids with no key
    /// here are ignored.
    pub fn press(&mut self, id: u8, t: u64) {
        self.tick(t);
        let Some(i) = index(id) else { return };
        let Some(p) = self.pos[i] else { return };
        self.heat[i] = 1.0;
        for (j, q) in self.pos.iter().enumerate() {
            let Some(q) = q else { continue };
            let d = dist(p, *q);
            if j == i || d >= SPREAD_RADIUS {
                continue;
            }
            let add = self.spread * ((SPREAD_RADIUS - d) / (SPREAD_RADIUS - 1.0)).min(1.0);
            self.heat[j] = (self.heat[j] + add).min(1.0);
        }
        let k = self.last_press.map(|l| (-(t.saturating_sub(l) as f64) / CENTRE_TAU_MS).exp()).unwrap_or(0.0);
        self.sums = (self.sums.0 * k + p.0, self.sums.1 * k + p.1, self.sums.2 * k + 1.0);
        self.centre = (self.sums.0 / self.sums.2, self.sums.1 / self.sums.2);
        self.weight += (1.0 - self.weight) * WEIGHT_GAIN;
        self.last_press = Some(t);
    }

    pub fn heat(&self, id: u8) -> f64 {
        index(id).map(|i| self.heat[i]).unwrap_or(0.0)
    }

    pub fn centre(&self) -> (f64, f64) {
        self.centre
    }

    pub fn weight(&self) -> f64 {
        self.weight
    }

    pub fn time(&self) -> u64 {
        self.t
    }

    /// Some key has heat (a press is still fading).
    pub fn has_heat(&self) -> bool {
        self.heat.iter().any(|h| *h > 0.0)
    }

    /// Nothing warm: every key shows exactly its static colour.
    pub fn is_idle(&self) -> bool {
        self.weight == 0.0 && self.heat.iter().all(|h| *h == 0.0)
    }

    /// The drift at a key centre: `drift` x the centre's weight x max(0, 1 - distance / [`DRIFT_RADIUS`]).
    fn drift_at(&self, drift: f64, p: (f64, f64)) -> f64 {
        drift * self.weight * (1.0 - dist(p, self.centre) / DRIFT_RADIUS).max(0.0)
    }

    /// Every key's sent colour now, in the layout's order (all 91, as `lightctl::compose`): the key's sent
    /// colour blended toward the sent warm colour by clamp(heat x [`HEAT_GAIN`] + drift), and above heat
    /// [`PEAK_FROM`] the warm colour itself leans toward the peak colour. Awareness keys and ids with no key
    /// keep their sent colour.
    pub fn frame(&self, s: &LightingSettings) -> Vec<(u8, Rgb)> {
        let r = &s.reactive;
        let warm = sent(s, parse_rgb(&r.warm).unwrap_or_else(|_| parse_rgb(DEFAULT_WARM).expect("a colour")));
        let peak = sent(s, parse_rgb(&r.peak).unwrap_or_else(|_| parse_rgb(DEFAULT_PEAK).expect("a colour")));
        let drift = r.drift.clamp(0.0, MAX_DRIFT);
        compose(s)
            .into_iter()
            .enumerate()
            .map(|(i, (id, c))| {
                let base = s.led(c);
                let Some(p) = self.pos[i] else { return (id, base) };
                if AWARENESS.contains(&id) {
                    return (id, base);
                }
                let h = self.heat[i];
                let f = (h * HEAT_GAIN + self.drift_at(drift, p)).clamp(0.0, 1.0);
                let target = if h > PEAK_FROM { mix(warm, peak, (h - PEAK_FROM) / (1.0 - PEAK_FROM) * PEAK_MIX) } else { warm };
                (id, mix(base, target, f))
            })
            .collect()
    }
}

/// What the static settings send per key (the idle frame): `compose` through the LED palette and balance.
pub fn static_frame(s: &LightingSettings) -> Vec<(u8, Rgb)> {
    compose(s).into_iter().map(|(id, c)| (id, s.led(c))).collect()
}

/// The keys of `next` whose colour moved more than `threshold` units (any channel) from `prev`, or that `prev`
/// doesn't have, in `next`'s order.
pub fn delta(prev: &[(u8, Rgb)], next: &[(u8, Rgb)], threshold: u8) -> Vec<(u8, Rgb)> {
    next.iter().filter(|(id, c)| prev.iter().find(|(p, _)| p == id).is_none_or(|(_, pc)| diff(*pc, *c) > threshold)).copied().collect()
}

// ---------------------------------------------------------------------------------------------------------
// Running it.

/// Why the glow can't run with these settings in this power state (`reactive.on` aside): it needs custom
/// mode, static colours and a lit keyboard.
pub fn eligible(s: &LightingSettings, p: &PowerState) -> Result<(), String> {
    s.validate()?;
    if s.mode != Mode::Custom {
        return Err("lighting is AWCC's (mode awcc), so the app writes nothing: switch to custom for the reactive glow".into());
    }
    if s.keyboard.effect != KeyEffect::Static {
        return Err(format!("the reactive glow needs Static colours; {} runs on the keyboard itself", s.keyboard.effect.name()));
    }
    if lightctl::level(s, p) == 0 {
        return Err("the keyboard is dark (off, brightness 0, or a sleep / lid / display rule)".into());
    }
    Ok(())
}

/// The glow is on and can run.
pub fn wanted(s: &LightingSettings, p: &PowerState) -> bool {
    s.reactive.on && eligible(s, p).is_ok()
}

/// The full frame from `prev`, as `lightctl::plan_from` sends static colours: "effect off" only when leaving
/// an effect, the 91 keys, and a brightness report only when the level isn't 100 % or wasn't before.
pub fn full_plan(prev: Shown, frame: &[(u8, Rgb)], s: &LightingSettings, p: &PowerState) -> Result<Vec<V5Step>, String> {
    let lvl = lightctl::level(s, p);
    let keys: Vec<Key> = frame.iter().map(|(id, c)| Key::new(*id, *c)).collect();
    let mut out = Vec::new();
    if prev.effect {
        out.extend(v5::global_effect(GlobalEffect::Off));
    }
    out.extend(v5::apply_keys(&keys).map_err(|e| e.to_string())?);
    if lvl != 100 || prev.level != 100 {
        out.extend(v5::brightness(lightctl::brightness_byte(lvl)));
    }
    Ok(out)
}

/// What a run sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Reports of the first, full frame.
    pub first: usize,
    /// Delta frames sent (frames with nothing changed send nothing and don't count).
    pub frames: usize,
    /// Reports in the delta frames.
    pub reports: usize,
    /// Key colours in the delta frames.
    pub keys: usize,
    /// The most reports one delta frame took.
    pub max_frame_reports: usize,
}

/// The glow running on one keyboard: AWCC paused for the whole run (its own lease, never timed out), the
/// keyboard opened once, and what was last sent per key.
pub struct Runner {
    settings: LightingSettings,
    power: PowerState,
    state: ReactiveState,
    dev: Box<dyn LightDevice>,
    sent: Vec<(u8, Rgb)>,
    lease: PauseLease,
    next_ms: u64,
    last_send_ms: u64,
    pub stats: Stats,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner").field("stats", &self.stats).field("next_ms", &self.next_ms).finish()
    }
}

impl Runner {
    /// Starts at `now` (ms): checks [`eligible`], pauses AWCC through `pauser` when given (held until
    /// [`Runner::stop`] / [`Runner::abandon`]), and writes the full frame from `prev`. A pause that fails sends
    /// nothing; a write that fails resumes AWCC and is an error.
    pub fn start(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, now: u64) -> Result<Runner, String> {
        eligible(s, p)?;
        let state = ReactiveState::new(&s.reactive, now);
        let frame = state.frame(s);
        let steps = full_plan(prev, &frame, s, p)?;
        check_steps(Api::V5, &steps)?;
        let mut dev = io.keyboard()?;
        let mut lease = PauseLease::default();
        if let Some(pz) = pauser {
            lease.acquire(pz, now)?;
        }
        let log = run_steps(dev.as_mut(), Api::V5, &steps).map_err(|e| format!("the reactive glow couldn't start: {e}"))?;
        let next_ms = now + s.reactive.interval_ms();
        Ok(Runner { settings: s.clone(), power: *p, state, dev, sent: frame, lease, next_ms, last_send_ms: now, stats: Stats { first: log.sent, ..Default::default() } })
    }

    /// A key-down of LED `id` at `now`.
    pub fn press(&mut self, id: u8, now: u64) {
        self.state.press(id, now);
    }

    /// When the next frame may go out, ms.
    pub fn due_ms(&self) -> u64 {
        self.next_ms
    }

    pub fn settings(&self) -> &LightingSettings {
        &self.settings
    }

    pub fn state(&self) -> &ReactiveState {
        &self.state
    }

    /// What AWCC's pause holds.
    pub fn paused(&self) -> Vec<String> {
        self.lease.paused()
    }

    /// One frame at `now`, if one is due (at most `fps` a second): the keys that changed by more than
    /// [`DELTA_THRESHOLD`] since they were last sent, largest first, at most [`MAX_KEYS_PER_FRAME`]. While no
    /// key is warm and only the drift fades, [`DRIFT_THRESHOLD`] and at most one frame per
    /// [`DRIFT_INTERVAL_MS`]; once everything is idle, any difference, so rest is exactly the static colours.
    /// Returns the reports sent (0 when not due or nothing changed). On an error nothing more is sent: call
    /// [`Runner::fail`].
    pub fn step(&mut self, now: u64) -> Result<usize, String> {
        if now < self.next_ms {
            return Ok(0);
        }
        self.next_ms = now + self.settings.reactive.interval_ms();
        self.state.tick(now);
        let threshold = if self.state.has_heat() {
            DELTA_THRESHOLD
        } else if now < self.last_send_ms + DRIFT_INTERVAL_MS {
            return Ok(0);
        } else if self.state.is_idle() {
            0
        } else {
            DRIFT_THRESHOLD
        };
        let next = self.state.frame(&self.settings);
        let mut changed = delta(&self.sent, &next, threshold);
        if changed.is_empty() {
            return Ok(0);
        }
        let moved = |(id, c): &(u8, Rgb)| self.sent.iter().find(|(p, _)| p == id).map(|(_, pc)| diff(*pc, *c)).unwrap_or(u8::MAX);
        changed.sort_by_key(|k| std::cmp::Reverse(moved(k)));
        changed.truncate(MAX_KEYS_PER_FRAME);
        let keys: Vec<Key> = changed.iter().map(|(id, c)| Key::new(*id, *c)).collect();
        let steps = frame_steps(self.settings.reactive.frame, &keys)?;
        let log = run_steps(self.dev.as_mut(), Api::V5, &steps)?;
        for (id, c) in &changed {
            if let Some(slot) = self.sent.iter_mut().find(|(p, _)| p == id) {
                slot.1 = *c;
            }
        }
        self.last_send_ms = now;
        self.stats.frames += 1;
        self.stats.reports += log.sent;
        self.stats.keys += changed.len();
        self.stats.max_frame_reports = self.stats.max_frame_reports.max(log.sent);
        Ok(log.sent)
    }

    /// The keyboard was just written with static settings `s` by someone else (the app's own write): carry on
    /// from there with `s`, the heat kept. Fails (and the caller drops the runner) when the glow can't run with
    /// `s` in `p`.
    pub fn resync(&mut self, s: &LightingSettings, p: &PowerState) -> Result<(), String> {
        eligible(s, p)?;
        self.settings = s.clone();
        self.power = *p;
        self.state.set_params(&s.reactive);
        self.sent = static_frame(s);
        Ok(())
    }

    /// Stops: the static settings written back as `lightctl::write` does (still inside AWCC's pause), then
    /// AWCC resumed.
    pub fn stop(mut self, io: &mut dyn LightIo) -> Result<Applied, String> {
        let r = lightctl::write_from(io, Shown::of(&self.settings, &self.power), &self.settings, &self.power, None);
        self.lease.release();
        r
    }

    /// Stops after `error` from [`Runner::step`]: the static settings written back, AWCC resumed. The text
    /// says what happened.
    pub fn fail(self, io: &mut dyn LightIo, error: String) -> String {
        match self.stop(io) {
            Ok(a) => format!("The reactive glow stopped: {error}. The static colours were written back: {}", a.text()),
            Err(e) => format!("The reactive glow stopped: {error}. Writing the static colours back failed too: {e}"),
        }
    }

    /// Stops without writing (the keyboard is going dark, or another write replaces the look); AWCC resumed.
    pub fn abandon(mut self) {
        self.lease.release();
    }
}

/// The clock for [`run`]: ms on a steady clock, and sleeping. Tests pass one that only counts.
pub trait Clock {
    fn now_ms(&mut self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// Ctrl+C was pressed.
    fn stopped(&self) -> bool {
        false
    }
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub stats: Stats,
    /// How long it ran, ms.
    pub ran_ms: u64,
    /// Reports sent in each second of the run (the full frame in the first).
    pub per_second: Vec<usize>,
    /// The write that put the static colours back.
    pub restore: Applied,
}

/// Runs the glow on `io` until `stop(now)` says so or the clock is stopped: each round takes the key-downs
/// `presses(now)` returns, sends a frame if one is due, and sleeps until the next. Stops cleanly: the static
/// settings written back and AWCC resumed, also after a failed write (then the error says so).
#[allow(clippy::too_many_arguments)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, clock: &mut dyn Clock, presses: &mut dyn FnMut(u64) -> Vec<u8>, stop: &mut dyn FnMut(u64) -> bool) -> Result<Summary, String> {
    let t0 = clock.now_ms();
    let mut r = Runner::start(io, s, p, prev, pauser, t0)?;
    let mut per_second = vec![r.stats.first];
    loop {
        let now = clock.now_ms();
        if clock.stopped() || stop(now) {
            break;
        }
        for id in presses(now) {
            r.press(id, now);
        }
        match r.step(now) {
            Ok(n) => {
                let sec = ((now - t0) / 1_000) as usize;
                if per_second.len() <= sec {
                    per_second.resize(sec + 1, 0);
                }
                per_second[sec] += n;
            }
            Err(e) => return Err(r.fail(io, e)),
        }
        let wait = r.due_ms().saturating_sub(clock.now_ms()).max(1);
        clock.sleep_ms(wait);
    }
    let ran_ms = clock.now_ms() - t0;
    let stats = r.stats.clone();
    let restore = r.stop(io).map_err(|e| format!("the reactive glow stopped, but writing the static colours back failed: {e}"))?;
    Ok(Summary { stats, ran_ms, per_second, restore })
}

/// Scripted typing for dry runs and tests: `text` at `per_second` keys a second from `start_ms`, each
/// character as the LED of its key (characters with no lit key, like the space bar, are skipped but take
/// their time). Returns (time ms, LED id).
pub fn script(text: &str, start_ms: u64, per_second: u32) -> Vec<(u64, u8)> {
    let step = 1_000 / per_second.max(1) as u64;
    text.chars()
        .enumerate()
        .filter_map(|(i, ch)| {
            let name = ch.to_ascii_uppercase().to_string();
            x17::by_name(&name).filter(|k| k.present).map(|k| (start_ms + i as u64 * step, k.id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A light frame is the changed colours, loop and update only: no reset (0x94) and no status read.
    #[test]
    fn light_frames_skip_reset_and_status() {
        let keys = [Key::new(0, Rgb::new(255, 0, 12)), Key::new(43, Rgb::new(0, 176, 224))];
        let full = frame_steps(FrameKind::Full, &keys).unwrap();
        let light = frame_steps(FrameKind::Light, &keys).unwrap();
        let ops = |steps: &[V5Step]| steps.iter().filter_map(|s| s.report()).map(|r| (r.as_bytes()[1], r.as_bytes()[2])).collect::<Vec<_>>();
        assert!(ops(&full).contains(&(0x94, 0x00)) && full.iter().any(|s| matches!(s, alienfx::Step::ReadStatus)));
        assert_eq!(ops(&light), vec![(0x8c, 0x02), (0x8c, 0x13), (0x8b, 0x01)]);
        assert!(!light.iter().any(|s| matches!(s, alienfx::Step::ReadStatus)));
        assert!(light.iter().filter_map(|s| s.report()).all(|r| alienfx::allowed(r.as_bytes(), Api::V5)));
        let quick = frame_steps(FrameKind::Quick, &keys).unwrap();
        assert_eq!(ops(&quick), vec![(0x94, 0x00), (0x8c, 0x02), (0x8c, 0x13), (0x8b, 0x01)], "quick: reset, colours, loop, update");
        assert!(!quick.iter().any(|s| matches!(s, alienfx::Step::ReadStatus)), "no status read");
        let mut r = ReactiveSettings::default();
        r.fps = 5;
        assert!(r.validate().is_ok(), "5 fps is allowed now");
        r.fps = 4;
        assert!(r.validate().is_err());
    }
    use crate::lightctl::FakeIo;
    use alienfx::device::FakePauser;

    fn custom() -> LightingSettings {
        let mut s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        s.reactive.on = true;
        s
    }

    /// The owner's kind of settings: a blue-ish base matched through the palette, peach in the palette.
    fn owner() -> LightingSettings {
        let mut s = custom();
        s.keyboard.color = "#6A3DFF".into();
        s.led_balance = lightctl::LedBalance::OWNER_X17;
        s.led_palette.insert("#6A3DFF".into(), "#3800FF".into());
        s.led_palette.insert("#FF9480".into(), "#FF5030".into());
        s.led_palette.insert("#FF6F61".into(), "#F83430".into());
        s.keyboard.keys.insert(40, "#000000".into());
        s.keyboard.keys.insert(0, "#FF1E3C".into());
        s
    }

    fn colour(frame: &[(u8, Rgb)], id: u8) -> Rgb {
        frame.iter().find(|(i, _)| *i == id).unwrap().1
    }

    const Q: u8 = 42;
    const W: u8 = 43;
    const A: u8 = 62;
    const H: u8 = 67;
    const P: u8 = 51;

    #[test]
    fn settings_default_validate_and_round_trip() {
        let r = ReactiveSettings::default();
        assert_eq!((r.on, r.fps, r.decay_ms, r.spread, r.drift, r.warm.as_str(), r.peak.as_str()), (false, 18, 700, 0.35, 0.12, "#FF9480", "#FF6F61"));
        assert!(r.validate().is_ok());
        assert_eq!(r.interval_ms(), 56);
        let bad = |f: &dyn Fn(&mut ReactiveSettings)| {
            let mut s = LightingSettings::default();
            f(&mut s.reactive);
            s.validate().unwrap_err()
        };
        assert!(bad(&|r| r.fps = 4).contains("5–30"));
        assert!(bad(&|r| r.fps = 31).contains("5–30"));
        assert!(bad(&|r| r.decay_ms = 100).contains("200–3000"));
        assert!(bad(&|r| r.decay_ms = 5_000).contains("200–3000"));
        assert!(bad(&|r| r.spread = 1.5).contains("spread"));
        assert!(bad(&|r| r.spread = f64::NAN).contains("spread"));
        assert!(bad(&|r| r.drift = -0.1).contains("drift"));
        assert!(bad(&|r| r.warm = "peach".into()).contains("warm colour"));
        assert!(bad(&|r| r.peak = "#12345".into()).contains("peak colour"));
        for fps in [10, 30] {
            let mut s = LightingSettings::default();
            s.reactive.fps = fps;
            assert!(s.validate().is_ok());
        }
        // In settings.json: camelCase, and missing (older files) or partial takes the defaults.
        let v = serde_json::to_value(LightingSettings::default()).unwrap();
        assert_eq!(v["reactive"], serde_json::json!({ "on": false, "fps": 18, "decayMs": 700, "spread": 0.35, "drift": 0.12, "warm": "#FF9480", "peak": "#FF6F61", "frame": "full" }));
        let old: LightingSettings = serde_json::from_str(r#"{ "mode": "custom" }"#).unwrap();
        assert_eq!(old.reactive, ReactiveSettings::default());
        let part: LightingSettings = serde_json::from_str(r#"{ "reactive": { "on": true, "fps": 24 } }"#).unwrap();
        assert_eq!((part.reactive.on, part.reactive.fps, part.reactive.decay_ms), (true, 24, 700));
    }

    #[test]
    fn heat_decays_exponentially() {
        let r = ReactiveSettings::default();
        let mut st = ReactiveState::new(&r, 0);
        st.press(H, 0);
        assert_eq!(st.heat(H), 1.0);
        st.tick(700);
        assert!((st.heat(H) - (-1.0f64).exp()).abs() < 1e-9, "one time constant: 1/e, {}", st.heat(H));
        st.tick(1_400);
        assert!((st.heat(H) - (-2.0f64).exp()).abs() < 1e-9);
        let mut a = ReactiveState::new(&r, 0);
        a.press(H, 0);
        for t in (0..=1_400).step_by(56) {
            a.tick(t);
        }
        assert!((a.heat(H) - st.heat(H)).abs() < 1e-9, "the same curve however often it ticks");
        st.tick(1_000);
        assert!((st.heat(H) - (-2.0f64).exp()).abs() < 1e-9, "time never runs backwards");
        st.tick(6_000);
        assert_eq!(st.heat(H), 0.0, "snapped to zero once it's under 1/1000");
        let mut slow = ReactiveState::new(&ReactiveSettings { decay_ms: 1_400, ..r }, 0);
        slow.press(H, 0);
        slow.tick(1_400);
        assert!((slow.heat(H) - (-1.0f64).exp()).abs() < 1e-9, "decay_ms is the time constant");
    }

    #[test]
    fn warmth_spreads_to_neighbours_and_falls_off() {
        let mut st = ReactiveState::new(&ReactiveSettings::default(), 0);
        st.press(H, 0);
        let g = 66; // G: one key left of H, the same row
        let j = 68; // J: one key right
        assert!((st.heat(g) - 0.35).abs() < 1e-9 && (st.heat(j) - 0.35).abs() < 1e-9, "a direct neighbour gets the full 0.35");
        let y = 47; // Y: the row above, 0.25 units left of H's centre (d = 1.03)
        assert!(st.heat(y) > 0.1 && st.heat(y) < 0.35, "a diagonal neighbour gets less: {}", st.heat(y));
        assert_eq!(st.heat(65), 0.0, "F is two keys away: nothing");
        assert_eq!(st.heat(P), 0.0);
        assert!(x17::KEYS.iter().filter(|k| k.present && st.heat(k.id) > 0.0).all(|k| dist(key_centre(k).unwrap(), key_centre(x17::key(H).unwrap()).unwrap()) < SPREAD_RADIUS), "nothing beyond 1.5 units");
        // Repeated presses add up but never pass 1.
        st.press(H, 10);
        st.press(H, 20);
        st.press(H, 30);
        assert_eq!(st.heat(g), 1.0);
        assert!(x17::KEYS.iter().all(|k| st.heat(k.id) <= 1.0));
        let mut none = ReactiveState::new(&ReactiveSettings { spread: 0.0, ..Default::default() }, 0);
        none.press(H, 0);
        assert_eq!(none.heat(g), 0.0, "spread 0: only the key itself");
        let before = none.clone();
        none.press(33, 0);
        none.press(200, 0);
        assert_eq!(none.heat(33), 0.0, "an id with no key here is ignored");
        assert_eq!((none.centre(), none.weight()), (before.centre(), before.weight()));
    }

    #[test]
    fn the_warm_centre_follows_typing_and_fades_when_idle() {
        let mut st = ReactiveState::new(&ReactiveSettings::default(), 0);
        assert_eq!(st.weight(), 0.0);
        st.press(Q, 0);
        let q = key_centre(x17::key(Q).unwrap()).unwrap();
        assert_eq!(st.centre(), q, "the first press puts the centre on the key");
        // Type on the right (P) for a while: the centre moves over, gradually.
        let p = key_centre(x17::key(P).unwrap()).unwrap();
        st.press(P, 200);
        let after_one = st.centre().0;
        assert!(after_one > q.0 && after_one < p.0, "one press moves it part of the way: {after_one}");
        for i in 2..60 {
            st.press(P, i * 200);
        }
        assert!((st.centre().0 - p.0).abs() < 0.1, "12 s of P: the centre is there: {:?}", st.centre());
        let w = st.weight();
        assert!(w > 0.6 && w <= 1.0, "steady typing builds the weight: {w}");
        // An 8 s moving average: a press long after the others moves it almost all the way.
        let mut far = st.clone();
        far.press(Q, 60 * 200 + 120_000);
        assert!((far.centre().0 - q.0).abs() < 0.01);
        // Idle: the weight fades with a 10 s time constant.
        st.tick(59 * 200 + 10_000);
        assert!((st.weight() - w * (-1.0f64).exp()).abs() < 1e-9);
        st.tick(59 * 200 + 100_000);
        assert_eq!(st.weight(), 0.0);
    }

    #[test]
    fn blend_endpoints_in_sent_space() {
        let s = owner();
        let st = ReactiveState::new(&s.reactive, 0);
        let base = Rgb::new(0x38, 0x00, 0xFF);
        assert_eq!(colour(&st.frame(&s), H), base, "heat 0: exactly the key's sent colour (the palette's match)");
        assert_eq!(st.frame(&s), static_frame(&s), "cold: exactly what the static write sends");
        let mut hot = st.clone();
        hot.press(H, 0);
        let c = colour(&hot.frame(&s), H);
        let warm = Rgb::new(0xFF, 0x50, 0x30);
        let coral = Rgb::new(0xF8, 0x34, 0x30);
        // heat 1: 85 % of the way from the base to the warm colour, leaning 40 % toward coral.
        let target = mix(warm, coral, PEAK_MIX);
        let want = mix(base, target, HEAT_GAIN + hot.drift_at(s.reactive.drift, key_centre(x17::key(H).unwrap()).unwrap()));
        assert_eq!(c, want);
        assert!(diff(c, warm) < 60 && diff(c, base) > 150, "about the warm colour: {c:?}");
        assert!(c.g < mix(base, warm, HEAT_GAIN).g + 1, "coral at the peak pulls green down");
        // The sent warm colour: the palette's, else the owner's match, else the balance.
        let mut plain = custom();
        assert_eq!(sent(&plain, Rgb::new(0xFF, 0x94, 0x80)), warm, "no palette: the owner's match");
        assert_eq!(sent(&plain, Rgb::new(0xFF, 0x6F, 0x61)), coral);
        plain.led_palette.insert("#ff9480".into(), "#FF6040".into());
        assert_eq!(sent(&plain, Rgb::new(0xFF, 0x94, 0x80)), Rgb::new(0xFF, 0x60, 0x40), "the palette wins");
        assert_eq!(sent(&plain, Rgb::new(0x12, 0x34, 0x56)), Rgb::new(0x12, 0x34, 0x56), "anything else: led()");
        assert_eq!(mix(base, warm, 0.0), base);
        assert_eq!(mix(base, warm, 1.0), warm);
    }

    #[test]
    fn awareness_keys_never_change() {
        let s = owner();
        let mut st = ReactiveState::new(&s.reactive, 0);
        let rest = st.frame(&s);
        for id in [0, 35, 15, 19, 40, Q, 21, 42, 14, 32, 55, 18] {
            st.press(id, 0);
        }
        for t in (0..2_000).step_by(50) {
            st.press(W, t);
            let f = st.frame(&s);
            for id in AWARENESS {
                assert_eq!(colour(&f, id), colour(&rest, id), "key {id} at {t} ms");
            }
        }
        assert!(st.heat(0) > 0.0, "the heat is there, the colour isn't shown");
        assert_ne!(colour(&st.frame(&s), Q), colour(&rest, Q), "Tab's neighbour Q does warm up");
        for id in [33, 59, 73, 82, 105, 113, 115] {
            assert_eq!(colour(&st.frame(&s), id), colour(&rest, id), "id {id} has no key");
        }
    }

    #[test]
    fn idle_returns_to_the_base_colours_exactly() {
        let s = owner();
        let mut st = ReactiveState::new(&s.reactive, 0);
        for (t, id) in script("the quick brown fox jumps over the lazy dog", 0, 8) {
            st.press(id, t);
        }
        assert_ne!(st.frame(&s), static_frame(&s));
        st.tick(10_000);
        assert_eq!(st.heat(A), 0.0, "heat is gone after a few seconds");
        assert!(st.weight() > 0.0, "the drift lingers");
        st.tick(120_000);
        assert!(st.is_idle());
        assert_eq!(st.frame(&s), static_frame(&s), "every key exactly its static sent colour");
        let mut dim = s.clone();
        dim.keyboard.keys.insert(H as u32, "#123456".into());
        assert_eq!(st.frame(&dim), static_frame(&dim), "own key colours too");
    }

    #[test]
    fn the_drift_leans_keys_near_the_centre() {
        let mut s = owner();
        let mut st = ReactiveState::new(&s.reactive, 0);
        for i in 0..50 {
            st.press(H, i * 150);
        }
        st.tick(50 * 150 + 5_000); // the heat is gone, the drift isn't
        let f = st.frame(&s);
        let base = Rgb::new(0x38, 0x00, 0xFF);
        let near = colour(&f, H);
        let mid = colour(&f, A);
        let far = colour(&f, 135);
        assert!(diff(near, base) > diff(mid, base) && diff(mid, base) >= diff(far, base), "{near:?} {mid:?} {far:?}");
        assert!(diff(near, base) <= (0.12 * 255.0_f64).ceil() as u8 + 1, "the drift is only a lean: {near:?}");
        assert_eq!(colour(&f, 135), base, "the right arrow is over 6 units from H");
        s.reactive.drift = 0.0;
        assert_eq!(st.frame(&s), static_frame(&s), "drift 0: no lean");
    }

    #[test]
    fn delta_sends_only_real_changes() {
        let a = vec![(1, Rgb::new(10, 10, 10)), (2, Rgb::new(20, 20, 20)), (3, Rgb::new(30, 30, 30))];
        let b = vec![(1, Rgb::new(13, 10, 10)), (2, Rgb::new(20, 24, 20)), (3, Rgb::new(30, 30, 30)), (4, Rgb::BLACK)];
        assert_eq!(delta(&a, &b, 3), vec![(2, Rgb::new(20, 24, 20)), (4, Rgb::BLACK)], "3 units is not a change, 4 is; a new key is");
        assert_eq!(delta(&a, &b, 0), vec![(1, Rgb::new(13, 10, 10)), (2, Rgb::new(20, 24, 20)), (4, Rgb::BLACK)]);
        assert!(delta(&b, &b, 0).is_empty());
        // A fading key: frames 56 ms apart don't all send it, but its colour never strays over the threshold
        // from what was sent.
        let s = owner();
        let mut st = ReactiveState::new(&s.reactive, 0);
        st.press(H, 0);
        let mut shown = st.frame(&s);
        let mut sends = 0;
        for t in (56..3_000).step_by(56) {
            st.tick(t);
            let next = st.frame(&s);
            let d = delta(&shown, &next, DELTA_THRESHOLD);
            sends += d.iter().filter(|(id, _)| *id == H).count();
            for (id, c) in d {
                shown.iter_mut().find(|(p, _)| *p == id).unwrap().1 = c;
            }
            assert!(diff(colour(&shown, H), colour(&next, H)) <= DELTA_THRESHOLD);
        }
        assert!(sends > 5 && sends < 53, "some frames, not all: {sends}");
    }

    fn reports(io: &FakeIo) -> usize {
        io.keyboard.sent().len()
    }

    #[derive(Default)]
    struct Fake {
        t: u64,
        sleeps: usize,
    }
    impl Clock for Fake {
        fn now_ms(&mut self) -> u64 {
            self.t
        }
        fn sleep_ms(&mut self, ms: u64) {
            self.t += ms;
            self.sleeps += 1;
        }
    }

    #[test]
    fn the_runner_sends_the_full_frame_then_small_deltas() {
        let s = owner();
        let io = FakeIo::default();
        let p = FakePauser::default();
        let mut r = Runner::start(&mut io.clone(), &s, &PowerState::default(), Shown::of(&s, &PowerState::default()), Some(&p), 0).unwrap();
        let full = lightctl::plan_from(Shown::of(&s, &PowerState::default()), &s, &PowerState::default()).unwrap();
        let want: Vec<Vec<u8>> = full.iter().filter_map(|x| x.report().map(|r| r.as_bytes().to_vec())).collect();
        assert_eq!(io.keyboard.sent(), want, "the first frame is exactly the static write");
        assert_eq!(r.stats.first, 11, "reset, status, 7 colorSets, loop, update = 11 reports");
        assert_eq!((p.paused(), p.resumed()), (1, 0), "AWCC paused for the whole run");
        assert_eq!(r.step(10).unwrap(), 0, "not due yet");
        assert_eq!(r.step(56).unwrap(), 0, "due, but nothing changed: nothing sent");
        io.keyboard.clear();
        r.press(H, 60);
        let n = r.step(112).unwrap();
        assert_eq!(n, 5, "one press: reset, status, one colorSet, loop, update");
        let sent = io.keyboard.sent();
        let sets: Vec<&Vec<u8>> = sent.iter().filter(|r| r[1] == 0x8c && r[2] == 0x02).collect();
        assert_eq!(sets.len(), 1);
        let ids: Vec<u8> = sets[0][4..].chunks(4).take_while(|b| b[0] != 0).map(|b| b[0] - 1).collect();
        assert!(ids.contains(&H) && ids.len() <= 9, "H and its neighbours only: {ids:?}");
        assert!(sent.iter().all(|r| alienfx::allowed(r, Api::V5) && !alienfx::is_memory_write(r, Api::V5)));
        // A burst over the whole keyboard: never more than two colorSets in a frame.
        io.keyboard.clear();
        for k in x17::KEYS.iter().filter(|k| k.present) {
            r.press(k.id, 150);
        }
        let n = r.step(170).unwrap();
        assert_eq!(n, 6, "30 keys: two colorSets");
        assert_eq!(r.step(226).unwrap(), 6, "the rest catch up next frame");
        assert_eq!(r.stats.max_frame_reports, 6);
        // Stop: the static settings back, AWCC resumed.
        io.keyboard.clear();
        let a = r.stop(&mut io.clone()).unwrap();
        assert_eq!(io.keyboard.sent(), want, "the static write");
        assert_eq!(a.keyboard, 11);
        assert_eq!((p.paused(), p.resumed()), (1, 1));
    }

    #[test]
    fn the_runner_refuses_what_it_cant_run_and_resyncs() {
        let p = PowerState::default();
        let io = FakeIo::default();
        let mut s = owner();
        s.mode = Mode::Awcc;
        assert!(Runner::start(&mut io.clone(), &s, &p, Shown::AWCC, None, 0).unwrap_err().contains("mode awcc"));
        s.mode = Mode::Custom;
        s.keyboard.effect = KeyEffect::Wave;
        assert!(Runner::start(&mut io.clone(), &s, &p, Shown::AWCC, None, 0).unwrap_err().contains("Static"));
        s.keyboard.effect = KeyEffect::Static;
        assert!(Runner::start(&mut io.clone(), &s, &PowerState { lid_closed: true, ..p }, Shown::AWCC, None, 0).unwrap_err().contains("dark"));
        s.reactive.fps = 4;
        assert!(Runner::start(&mut io.clone(), &s, &p, Shown::AWCC, None, 0).unwrap_err().contains("5–30"));
        assert!(io.keyboard.sent().is_empty(), "nothing sent");
        assert!(!wanted(&custom_off(), &p) && wanted(&owner(), &p));
        let pz = FakePauser::default();
        pz.fail();
        assert!(Runner::start(&mut io.clone(), &owner(), &p, Shown::AWCC, Some(&pz), 0).unwrap_err().contains("nothing was sent"));
        assert!(io.keyboard.sent().is_empty());

        // Dimmed: the full frame keeps the brightness; deltas never touch it.
        let mut dim = owner();
        dim.keyboard.brightness = 40;
        let mut r = Runner::start(&mut io.clone(), &dim, &p, Shown::AWCC, None, 0).unwrap();
        assert_eq!(io.keyboard.sent().last().unwrap()[1], 0x83, "from AWCC's 100 %: brightness last");
        // Resync after the app's own write of new settings: carry on from the static colours.
        let mut s2 = owner();
        s2.keyboard.color = "#00E5FF".into();
        r.resync(&s2, &p).unwrap();
        assert_eq!(r.settings().keyboard.color, "#00E5FF");
        assert_eq!(r.step(56).unwrap(), 0, "the keyboard shows s2's static colours already");
        let mut off = s2.clone();
        off.keyboard.on = false;
        assert!(r.resync(&off, &p).is_err());
        r.abandon();
    }

    fn custom_off() -> LightingSettings {
        let mut s = owner();
        s.reactive.on = false;
        s
    }

    #[test]
    fn run_caps_frames_at_fps_and_restores_on_stop() {
        let s = owner();
        let io = FakeIo::default();
        let p = FakePauser::default();
        let presses = script("the quick brown fox jumps over the lazy dog the quick brown fox", 0, 10);
        let mut given = 0;
        let mut clock = Fake::default();
        let sum = run(
            &mut io.clone(),
            &s,
            &PowerState::default(),
            Shown::AWCC,
            Some(&p),
            &mut clock,
            &mut |now| {
                let mut out = vec![];
                while given < presses.len() && presses[given].0 <= now {
                    out.push(presses[given].1);
                    given += 1;
                }
                out
            },
            &mut |now| now >= 70_000,
        )
        .unwrap();
        assert_eq!(given, presses.len());
        assert!(sum.stats.frames <= 70 * 18 && sum.stats.frames > 6 * 18, "at most 18 frames a second: {}", sum.stats.frames);
        assert!(clock.sleeps <= 70 * 18 + 1, "the loop sleeps until the next frame: {}", clock.sleeps);
        assert!(sum.stats.max_frame_reports <= 6);
        assert!(sum.per_second[..6].iter().all(|n| *n > 0), "typing: reports every second {:?}", sum.per_second);
        assert_eq!(sum.per_second.iter().sum::<usize>(), sum.stats.first + sum.stats.reports);
        assert!(sum.per_second[12..].iter().all(|n| *n <= 12), "only the drift fading: at most two small frames a second {:?}", sum.per_second);
        assert!(sum.per_second[60..].iter().all(|n| *n == 0), "idle: nothing at all {:?}", sum.per_second);
        assert_eq!(sum.restore.keyboard, 11, "the static settings written back");
        assert_eq!((p.paused(), p.resumed()), (1, 1));
        let last10: Vec<Vec<u8>> = io.keyboard.sent().iter().rev().take(11).rev().cloned().collect();
        let want: Vec<Vec<u8>> = lightctl::plan(&s, &PowerState::default()).unwrap().iter().filter_map(|x| x.report().map(|r| r.as_bytes().to_vec())).collect();
        assert_eq!(last10, want);

        // A faster setting sends more often, never more than its fps.
        let mut fast = s.clone();
        fast.reactive.fps = 30;
        let mut clock = Fake::default();
        let sum = run(&mut FakeIo::default(), &fast, &PowerState::default(), Shown::AWCC, None, &mut clock, &mut |now| if now % 100 < 34 { vec![H, P] } else { vec![] }, &mut |now| now >= 2_000).unwrap();
        assert!(sum.stats.frames <= 2 * 30 && sum.stats.frames >= 40, "{}", sum.stats.frames);
    }

    #[test]
    fn a_failed_write_stops_and_restores() {
        let s = owner();
        let io = FakeIo::default();
        let p = FakePauser::default();
        let mut clock = Fake::default();
        io.keyboard.fail_at(11 + 2); // the full frame is 11 reports; the first delta frame fails on its colorSet
        let mut first = true;
        let e = run(&mut io.clone(), &s, &PowerState::default(), Shown::AWCC, Some(&p), &mut clock, &mut |_| if std::mem::take(&mut first) { vec![H] } else { vec![] }, &mut |now| now > 10_000).unwrap_err();
        assert!(e.contains("failed on demand") && e.contains("static colours were written back"), "{e}");
        assert!(clock.t < 1_000, "stopped at once");
        assert_eq!((p.paused(), p.resumed()), (1, 1), "AWCC resumed");
        let want: Vec<Vec<u8>> = lightctl::plan(&s, &PowerState::default()).unwrap().iter().filter_map(|x| x.report().map(|r| r.as_bytes().to_vec())).collect();
        let sent = io.keyboard.sent();
        assert_eq!(sent[sent.len() - 11..], want[..], "the static write came last");
        // The first write failing: nothing to restore, AWCC resumed.
        let io2 = FakeIo::default();
        io2.keyboard.fail_at(0);
        let p2 = FakePauser::default();
        assert!(Runner::start(&mut io2.clone(), &s, &PowerState::default(), Shown::AWCC, Some(&p2), 0).unwrap_err().contains("couldn't start"));
        assert_eq!((p2.paused(), p2.resumed()), (1, 1));
        let _ = reports(&io2);
    }

    #[test]
    fn script_types_present_keys_only() {
        let s = script("ab c", 1_000, 4);
        assert_eq!(s, vec![(1_000, 62), (1_250, 87), (1_750, 85)], "the space takes its time, sends nothing");
    }
}
