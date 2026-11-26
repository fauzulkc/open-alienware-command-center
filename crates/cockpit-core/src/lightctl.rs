//! Keyboard lighting control, as docs/lighting-control.md specifies it: the settings, their validation, what
//! the keyboard shows for them ([`compose`]), the reports a write sends ([`plan`], [`zone_plan`],
//! [`restore_awcc_plan`]), AWCC's look per key ([`awcc_keys`], [`import_awcc`]), the power rules
//! ([`on_power_event`]) and the write itself ([`apply`], [`write`]).
//!
//! Everything here is pure except [`apply`] / [`write`] / [`restore`], which go through a [`LightIo`]: the
//! real devices on Windows ([`SystemIo`]) or [`FakeIo`] in tests. Every report passes `alienfx::allowed`, and
//! controller-memory writes are refused before anything is sent (`alienfx::device::run_steps`).
//!
//! In mode `awcc` the app never writes; [`on_power_event`] answers [`LightAction::Nothing`] and the callers
//! check the mode before [`write`]. `restore-awcc` is the one write that puts AWCC's own look back.

use crate::lighting;
pub use alienfx;
use alienfx::device::{check_steps, run_steps, FakeDevice, FakePauser, LightDevice, Pauser, RunLog};
use alienfx::v4::{self, V4Step};
use alienfx::v5::{self, ColorCount, EffectKind, GlobalEffect, Key, V5Step};
use alienfx::{x17, Api, Rgb, Step};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------------------
// Settings.

/// Who owns the keyboard's look.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// AWCC's look stays; the app never writes.
    #[default]
    Awcc,
    /// The app writes these settings.
    Custom,
}

/// `static` is per-key colours; the others are the keyboard's own (v5 global) effects.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum KeyEffect {
    #[default]
    Static,
    Breathing,
    Wave,
    DualWave,
    Pulse,
    MixPulse,
    NightRider,
    Laser,
}

impl KeyEffect {
    pub const ALL: [KeyEffect; 8] = [KeyEffect::Static, KeyEffect::Breathing, KeyEffect::Wave, KeyEffect::DualWave, KeyEffect::Pulse, KeyEffect::MixPulse, KeyEffect::NightRider, KeyEffect::Laser];

    /// The name as the settings file and the app spell it ("dualWave").
    pub fn name(self) -> &'static str {
        match self {
            KeyEffect::Static => "static",
            KeyEffect::Breathing => "breathing",
            KeyEffect::Wave => "wave",
            KeyEffect::DualWave => "dualWave",
            KeyEffect::Pulse => "pulse",
            KeyEffect::MixPulse => "mixPulse",
            KeyEffect::NightRider => "nightRider",
            KeyEffect::Laser => "laser",
        }
    }

    /// By name, ignoring case, dashes and underscores ("dual-wave", "NightRider").
    pub fn parse(s: &str) -> Option<KeyEffect> {
        let t: String = s.chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect::<String>().to_ascii_lowercase();
        KeyEffect::ALL.into_iter().find(|e| e.name().to_ascii_lowercase() == t)
    }

    /// The v5 global effect type; `None` for `static`.
    pub fn kind(self) -> Option<EffectKind> {
        match self {
            KeyEffect::Static => None,
            KeyEffect::Breathing => Some(EffectKind::Breathing),
            KeyEffect::Wave => Some(EffectKind::SingleColorWave),
            KeyEffect::DualWave => Some(EffectKind::DualColorWave),
            KeyEffect::Pulse => Some(EffectKind::Pulse),
            KeyEffect::MixPulse => Some(EffectKind::MixPulse),
            KeyEffect::NightRider => Some(EffectKind::NightRider),
            KeyEffect::Laser => Some(EffectKind::Laser),
        }
    }

    /// Effects that use `color2` as well.
    pub fn two_colours(self) -> bool {
        matches!(self, KeyEffect::DualWave | KeyEffect::MixPulse)
    }
}

/// The keyboard's backlight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct KeyboardLight {
    /// false = backlight off (brightness 0), colours kept.
    pub on: bool,
    /// 0..=100 %.
    pub brightness: u32,
    pub effect: KeyEffect,
    /// 0..=255, effects only: passed through as alien-tools sends it (`COMMV5_setEffect` byte 3). Which way
    /// is faster is not known yet.
    pub tempo: u32,
    /// Base colour for every key, `#RRGGBB`.
    pub color: String,
    /// Second colour for dualWave / mixPulse.
    pub color2: String,
    /// Group name -> colour, over the base.
    pub groups: BTreeMap<String, String>,
    /// Key id -> colour, over groups.
    pub keys: BTreeMap<u32, String>,
}

/// AWCC's own default keyboard colour ("Static Default Blue", ARGB 0xFF00F0F0 in its FXMetadata).
pub const AWCC_DEFAULT_COLOR: &str = "#00F0F0";

impl Default for KeyboardLight {
    fn default() -> Self {
        KeyboardLight { on: true, brightness: 100, effect: KeyEffect::Static, tempo: 128, color: AWCC_DEFAULT_COLOR.into(), color2: "#FF00FF".into(), groups: BTreeMap::new(), keys: BTreeMap::new() }
    }
}

/// The keyboard's LED colour balance, % per channel (10..=100). The x17 R2's LEDs don't light red, green
/// and blue equally at equal values: on the owner's keyboard red and green are far stronger than blue, so
/// white comes out yellow, peach orange and lavender washed out. Measured 2026-11-24 by eye: neutral white
/// at red 50 %, green 60 %, blue 100 %. Every colour sent to the keyboard is multiplied by these, then
/// scaled back up so its strongest channel keeps its value (brightness stays). 100/100/100 sends colours
/// unchanged, as AWCC does. Settings and the UI keep the intended colours; only what is sent changes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LedBalance {
    pub r: u32,
    pub g: u32,
    pub b: u32,
}

impl Default for LedBalance {
    fn default() -> Self {
        LedBalance { r: 100, g: 100, b: 100 }
    }
}

impl LedBalance {
    /// The owner's keyboard, measured 2026-11-24.
    pub const OWNER_X17: LedBalance = LedBalance { r: 50, g: 60, b: 100 };

    pub fn is_identity(&self) -> bool {
        *self == LedBalance::default()
    }

    /// `c` as it should be sent: balanced, then scaled so the strongest channel keeps `c`'s strongest value.
    pub fn apply(&self, c: Rgb) -> Rgb {
        if self.is_identity() {
            return c;
        }
        let scaled = [c.r as f64 * self.r as f64 / 100.0, c.g as f64 * self.g as f64 / 100.0, c.b as f64 * self.b as f64 / 100.0];
        let max_out = scaled.iter().cloned().fold(0.0, f64::max);
        if max_out <= 0.0 {
            return Rgb::BLACK;
        }
        let k = c.r.max(c.g).max(c.b) as f64 / max_out;
        let ch = |v: f64| (v * k).round().clamp(0.0, 255.0) as u8;
        Rgb::new(ch(scaled[0]), ch(scaled[1]), ch(scaled[2]))
    }

    fn validate(&self) -> Result<(), String> {
        for (name, v) in [("red", self.r), ("green", self.g), ("blue", self.b)] {
            if !(10..=100).contains(&v) {
                return Err(format!("LED balance {name} {v} % is out of range: 10–100 %"));
            }
        }
        Ok(())
    }
}

/// Lighting, as the app and `cockpit lighting` share it (`lighting` in settings.json).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LightingSettings {
    pub mode: Mode,
    pub keyboard: KeyboardLight,
    /// v4 light id -> colour (sent, not visible on this laptop).
    pub zones: BTreeMap<u32, String>,
    /// Dark before sleep, restored on wake.
    pub off_on_sleep: bool,
    /// Dark with the lid closed.
    pub off_on_lid_close: bool,
    /// Dark when the display turns off.
    pub off_on_display_off: bool,
    /// The keyboard clock when the display turns off (instead of dark; `crate::live`). Missing in older
    /// files: off.
    #[serde(default)]
    pub clock_on_display_off: bool,
    /// Brightness 30 % while Windows Energy Saver is on.
    pub dim_on_battery_saver: bool,
    /// Suspend AWCC's lighting programs around each write.
    pub pause_awcc: bool,
    /// The keyboard LEDs' colour balance, applied to every colour this app sends ([`LedBalance`]).
    pub led_balance: LedBalance,
    /// Colours matched by eye on this keyboard: intended `#RRGGBB` (what the app shows) -> the value to send
    /// so the key looks like it. A plain balance can't do this (the LEDs don't mix linearly), so the owner
    /// picked each colour from candidates on the keys (2026-11-24). Colours not listed go through
    /// `led_balance`. Matching ignores case.
    pub led_palette: BTreeMap<String, String>,
    /// The reactive glow: keys warm up as they are pressed (`crate::reactive`). Missing in older files: off.
    #[serde(default)]
    pub reactive: crate::reactive::ReactiveSettings,
    /// A PC-driven animation the app keeps running (`crate::live`). Missing in older files: off.
    #[serde(default)]
    pub live: crate::live::LiveSettings,
    /// Brighter by day, dimmer at night, following the sun where you are (`crate::daylight`). Missing in
    /// older files: off.
    #[serde(default)]
    pub daylight: crate::daylight::DaylightSettings,
}

impl LightingSettings {
    /// The value to send for intended colour `c`: the matched palette entry, else the balanced colour.
    pub fn led(&self, c: Rgb) -> Rgb {
        let key = rgb_hex(c);
        self.led_palette.iter().find(|(k, _)| k.eq_ignore_ascii_case(&key)).and_then(|(_, v)| parse_rgb(v).ok()).unwrap_or_else(|| self.led_balance.apply(c))
    }
}

impl Default for LightingSettings {
    fn default() -> Self {
        LightingSettings { mode: Mode::Awcc, keyboard: KeyboardLight::default(), zones: BTreeMap::new(), off_on_sleep: true, off_on_lid_close: true, off_on_display_off: true, clock_on_display_off: false, dim_on_battery_saver: false, pause_awcc: true, led_balance: LedBalance::default(), led_palette: BTreeMap::new(), reactive: Default::default(), live: Default::default(), daylight: Default::default() }
    }
}

/// Highest v4 zone id the settings accept.
pub const MAX_ZONE_ID: u32 = 19;
/// Brightness while Energy Saver is on and `dimOnBatterySaver` is set, %.
pub const DIM_PCT: u32 = 30;

/// `#RRGGBB` (either case) to a colour.
pub fn parse_rgb(s: &str) -> Result<Rgb, String> {
    let bad = || format!("'{s}' isn't a colour: use #RRGGBB");
    let h = s.strip_prefix('#').filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(bad)?;
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| bad());
    Ok(Rgb::new(p(0)?, p(2)?, p(4)?))
}

pub fn rgb_hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// A key id as the settings carry it, if it is one of the 91.
pub fn key_id(id: u32) -> Option<u8> {
    u8::try_from(id).ok().filter(|i| x17::key(*i).is_some())
}

impl LightingSettings {
    /// The contract's rules: brightness 0..=100, tempo 0..=255, colours `#RRGGBB`, key ids only the 91
    /// known, group names only AWCC's 4, zone ids only 0..=19.
    pub fn validate(&self) -> Result<(), String> {
        self.led_balance.validate()?;
        self.reactive.validate()?;
        self.live.validate()?;
        self.daylight.validate().map_err(|e| format!("day and night: {e}"))?;
        for (from, to) in &self.led_palette {
            parse_rgb(from).and_then(|_| parse_rgb(to)).map_err(|e| format!("LED palette {from} -> {to}: {e}"))?;
        }
        let k = &self.keyboard;
        if k.brightness > 100 {
            return Err(format!("brightness {} is out of range: 0–100 %", k.brightness));
        }
        if k.tempo > 255 {
            return Err(format!("tempo {} is out of range: 0–255", k.tempo));
        }
        parse_rgb(&k.color).map_err(|e| format!("colour: {e}"))?;
        parse_rgb(&k.color2).map_err(|e| format!("second colour: {e}"))?;
        for (g, c) in &k.groups {
            if !x17::GROUPS.iter().any(|x| x.name == g) {
                return Err(format!("'{g}' isn't a key group: use {}", group_names().join(", ")));
            }
            parse_rgb(c).map_err(|e| format!("group {g}: {e}"))?;
        }
        for (id, c) in &k.keys {
            if key_id(*id).is_none() {
                return Err(format!("{id} isn't a key of this keyboard"));
            }
            parse_rgb(c).map_err(|e| format!("key {id}: {e}"))?;
        }
        for (id, c) in &self.zones {
            if *id > MAX_ZONE_ID {
                return Err(format!("zone {id} is out of range: 0–{MAX_ZONE_ID}"));
            }
            parse_rgb(c).map_err(|e| format!("zone {id}: {e}"))?;
        }
        Ok(())
    }
}

pub fn group_names() -> Vec<String> {
    x17::GROUPS.iter().map(|g| g.name.to_string()).collect()
}

// ---------------------------------------------------------------------------------------------------------
// What the keyboard shows.

/// Per key, in the layout's order: `keys[id] ?? groups[group] ?? color` (a key in two groups takes the later
/// group's colour, so W follows WASD over QWER). Assumes validated settings; an unreadable colour falls
/// through to the next layer, and an unreadable base is black.
pub fn compose(s: &LightingSettings) -> Vec<(u8, Rgb)> {
    let k = &s.keyboard;
    let base = parse_rgb(&k.color).unwrap_or(Rgb::BLACK);
    x17::KEYS
        .iter()
        .map(|key| {
            let own = k.keys.get(&(key.id as u32)).and_then(|c| parse_rgb(c).ok());
            let group = || x17::groups_of(key.id).collect::<Vec<_>>().into_iter().rev().find_map(|g| k.groups.get(g.name).and_then(|c| parse_rgb(c).ok()));
            (key.id, own.or_else(group).unwrap_or(base))
        })
        .collect()
}

/// What the power rules know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerState {
    pub suspended: bool,
    pub lid_closed: bool,
    pub display_off: bool,
    pub energy_saver: bool,
    /// The brightness the sun's position asks for now, %, while day and night is on (`crate::daylight`);
    /// `None` until the app has looked.
    pub daylight: Option<u32>,
}

/// A dark trigger is active (sleep, lid closed, display off, each by its setting; the display off shows the
/// clock instead when `clock_on_display_off`).
pub fn is_dark(s: &LightingSettings, p: &PowerState) -> bool {
    (p.suspended && s.off_on_sleep) || (p.lid_closed && s.off_on_lid_close) || (p.display_off && s.off_on_display_off && !s.clock_on_display_off)
}

/// The brightness to show, %: 0 when off or dark; the sun's level instead of the set brightness while day and
/// night is on; capped at [`DIM_PCT`] under Energy Saver when set.
pub fn level(s: &LightingSettings, p: &PowerState) -> u32 {
    if !s.keyboard.on || is_dark(s, p) {
        return 0;
    }
    let b = match (s.daylight.on, p.daylight) {
        (true, Some(d)) => d,
        _ => s.keyboard.brightness,
    }
    .min(100);
    if p.energy_saver && s.dim_on_battery_saver {
        b.min(DIM_PCT)
    } else {
        b
    }
}

/// The reports that change only the brightness, to `pct` (day and night's gradual steps): colours, effects and
/// a running live animation are left as they are.
pub fn level_plan(pct: u32) -> Vec<V5Step> {
    v5::brightness(brightness_byte(pct))
}

/// 0..=100 % to the v5 brightness byte 0..=255 (`COMMV5_turnOnSet` byte 4), rounded: 100 -> 255, 30 -> 77.
pub fn brightness_byte(pct: u32) -> u8 {
    ((pct.min(100) * 255 + 50) / 100) as u8
}

fn tempo_byte(t: u32) -> u8 {
    t.min(255) as u8
}

/// What the keyboard shows now, as far as this app knows: a hardware effect or per-key colours, and the
/// brightness %. [`Shown::AWCC`] is AWCC's own state (per-key colours at full brightness), assumed when
/// nothing better is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Shown {
    pub effect: bool,
    pub level: u32,
}

impl Shown {
    pub const AWCC: Shown = Shown { effect: false, level: 100 };

    /// What writing `s` in power state `p` leaves on the keyboard.
    pub fn of(s: &LightingSettings, p: &PowerState) -> Shown {
        let level = level(s, p);
        Shown { effect: level > 0 && s.keyboard.effect.kind().is_some(), level }
    }
}

impl Default for Shown {
    fn default() -> Self {
        Shown::AWCC
    }
}

/// The keyboard reports for these settings, from AWCC's own state ([`plan_from`] with [`Shown::AWCC`]).
pub fn plan(s: &LightingSettings, p: &PowerState) -> Result<Vec<V5Step>, String> {
    plan_from(Shown::AWCC, s, p)
}

/// The keyboard reports that take it from `prev` to these settings in this power state. Only what is needed
/// is sent; per-key colours at full brightness are exactly the sequence proven on the owner's keyboard
/// (2026-11-24: reset, status, colour sets, loop, update). The first on-device run of this feature, which
/// always added "effect off" and a brightness report, shifted reds, pinks and lightness on some keys.
/// - dark (off, brightness 0 or a dark trigger): brightness 0 only, colours untouched;
/// - `static`: "effect off" only when leaving a hardware effect, then the 91 keys ([`compose`]);
/// - an effect: the global effect with `color`/`color2`/`tempo`;
/// - then a brightness report only when the level isn't 100 %, or the keyboard was dimmed or dark before.
pub fn plan_from(prev: Shown, s: &LightingSettings, p: &PowerState) -> Result<Vec<V5Step>, String> {
    let lvl = level(s, p);
    if lvl == 0 {
        return Ok(v5::brightness(0));
    }
    let k = &s.keyboard;
    let mut out = Vec::new();
    match k.effect.kind() {
        None => {
            let keys: Vec<Key> = compose(s).into_iter().map(|(id, c)| Key::new(id, s.led(c))).collect();
            if prev.effect {
                out.extend(v5::global_effect(GlobalEffect::Off));
            }
            out.extend(v5::apply_keys(&keys).map_err(|e| e.to_string())?);
        }
        Some(kind) => {
            let colors = if k.effect.two_colours() { ColorCount::Two } else { ColorCount::One };
            let effect = GlobalEffect::Effect { kind, tempo: tempo_byte(k.tempo), colors, color1: s.led(parse_rgb(&k.color)?), color2: s.led(parse_rgb(&k.color2)?) };
            out.extend(v5::global_effect(effect));
        }
    }
    if lvl != 100 || prev.level != 100 {
        out.extend(v5::brightness(brightness_byte(lvl)));
    }
    Ok(out)
}

/// The zone controller's reports: each zone's colour, live (no stored state), then update. Nothing when there
/// are no zones or the keyboard is dark (the zones don't light on this laptop either way).
pub fn zone_plan(s: &LightingSettings, p: &PowerState) -> Result<Vec<V4Step>, String> {
    if s.zones.is_empty() || level(s, p) == 0 {
        return Ok(Vec::new());
    }
    let mut out = v4::reset();
    for (id, c) in &s.zones {
        let id = u8::try_from(*id).ok().filter(|i| *i as u32 <= MAX_ZONE_ID).ok_or_else(|| format!("zone {id} is out of range"))?;
        out.push(Step::Write(v4::set_one_color(id, parse_rgb(c)?)));
    }
    out.push(Step::Write(v4::update()));
    Ok(out)
}

/// One key in one colour, not saved (the map's live preview).
pub fn preview_plan(id: u32, color: &str) -> Result<Vec<V5Step>, String> {
    preview_plan_with(LedBalance::default(), id, color)
}

/// [`preview_plan`] through the keyboard's LED balance.
pub fn preview_plan_with(balance: LedBalance, id: u32, color: &str) -> Result<Vec<V5Step>, String> {
    let id = key_id(id).ok_or_else(|| format!("{id} isn't a key of this keyboard"))?;
    v5::apply_keys(&[Key::new(id, balance.apply(parse_rgb(color)?))]).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------------------
// AWCC's look.

/// AWCC's active keyboard preset, per key (a read-only copy of what AWCC set).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AwccLook {
    pub preset: String,
    /// Key id -> `#RRGGBB`.
    pub keys: BTreeMap<u32, String>,
    /// AWCC's brightness for the preset, % (the highest of its groups).
    #[serde(skip)]
    pub brightness: u32,
}

/// The keyboard's look from AWCC's active presets (`lighting::read`, as `cockpit --json lighting show`
/// prints it): each group's first colour on each of its LEDs that is one of the 91 keys. `None` when no
/// keyboard preset is there.
pub fn awcc_keys(devices: &[lighting::Device]) -> Option<AwccLook> {
    let kb = devices.iter().find(|d| {
        let n = d.device.to_ascii_lowercase();
        (n.contains("mechanical") || n.ends_with("kb")) && !d.groups.is_empty()
    })?;
    let mut keys = BTreeMap::new();
    for g in &kb.groups {
        let Some(c) = g.colors.first().and_then(|c| parse_rgb(c).ok()) else { continue };
        for &led in &g.leds {
            if key_id(led).is_some() {
                keys.insert(led, rgb_hex(c));
            }
        }
    }
    let brightness = kb.groups.iter().map(|g| g.brightness.clamp(0, 100) as u32).max().unwrap_or(100);
    Some(AwccLook { preset: kb.preset.clone(), keys, brightness })
}

/// AWCC's look written back: global effect off, all 91 keys in AWCC's colours (a key AWCC doesn't list is
/// dark), then AWCC's brightness.
pub fn restore_awcc_plan(look: &AwccLook) -> Result<Vec<V5Step>, String> {
    restore_awcc_plan_from(Shown::AWCC, look)
}

/// AWCC's look written back from `prev`: "effect off" only when leaving an effect, the 91 keys, and a
/// brightness report only when AWCC's brightness isn't 100 % or the keyboard was dimmed or dark (as in
/// [`plan_from`]).
pub fn restore_awcc_plan_from(prev: Shown, look: &AwccLook) -> Result<Vec<V5Step>, String> {
    let keys: Vec<Key> = x17::KEYS.iter().map(|k| Key::new(k.id, look.keys.get(&(k.id as u32)).and_then(|c| parse_rgb(c).ok()).unwrap_or(Rgb::BLACK))).collect();
    let mut out = Vec::new();
    if prev.effect {
        out.extend(v5::global_effect(GlobalEffect::Off));
    }
    out.extend(v5::apply_keys(&keys).map_err(|e| e.to_string())?);
    if look.brightness != 100 || prev.level != 100 {
        out.extend(v5::brightness(brightness_byte(look.brightness)));
    }
    Ok(out)
}

/// Custom settings seeded from AWCC's look, per key: the most common colour becomes the base, every other
/// key its own colour, no group colours, static, AWCC's brightness, on. The mode and the rest are kept.
pub fn import_awcc(current: &LightingSettings, look: &AwccLook) -> LightingSettings {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for c in look.keys.values() {
        *count.entry(c.as_str()).or_default() += 1;
    }
    let base = count.iter().max_by_key(|(c, n)| (**n, std::cmp::Reverse(**c))).map(|(c, _)| c.to_string()).unwrap_or_else(|| current.keyboard.color.clone());
    let keys = look.keys.iter().filter(|(_, c)| **c != base).map(|(id, c)| (*id, c.clone())).collect();
    let mut s = current.clone();
    s.keyboard = KeyboardLight { on: true, brightness: look.brightness.min(100), effect: KeyEffect::Static, color: base, groups: BTreeMap::new(), keys, ..current.keyboard.clone() };
    s
}

// ---------------------------------------------------------------------------------------------------------
// Power events.

/// A power change, decoded from Windows' broadcasts ([`broadcast_event`], [`setting_event`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    Suspend,
    Resume,
    Lid { open: bool },
    Display { on: bool },
    EnergySaver { on: bool },
    /// Day and night's level now (`None`: off).
    Daylight { pct: Option<u32> },
}

/// What to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightAction {
    Nothing,
    /// Write [`plan`] now (it is brightness 0).
    Dark,
    /// Write [`plan`] for the current settings after this long.
    Reapply { after_ms: u64 },
    /// Only the brightness changed (day and night): send [`level_plan`] for the new level, nothing else.
    Level,
}

/// The wait before re-applying after a resume: the keyboard needs a moment, and AWCC writes on wake.
pub const RESUME_DELAY_MS: u64 = 2_000;

/// Updates `state` and decides, by the contract's rules. Mode `awcc`: always nothing (the state is still
/// tracked). Resume: re-apply after [`RESUME_DELAY_MS`] (AWCC may have written meanwhile). Otherwise a
/// change of the shown brightness: to 0 is `Dark`, anything else re-applies now.
pub fn on_power_event(state: &mut PowerState, ev: PowerEvent, s: &LightingSettings) -> LightAction {
    let before = level(s, state);
    match ev {
        PowerEvent::Suspend => state.suspended = true,
        PowerEvent::Resume => state.suspended = false,
        PowerEvent::Lid { open } => state.lid_closed = !open,
        PowerEvent::Display { on } => state.display_off = !on,
        PowerEvent::EnergySaver { on } => state.energy_saver = on,
        PowerEvent::Daylight { pct } => state.daylight = pct,
    }
    if s.mode != Mode::Custom {
        return LightAction::Nothing;
    }
    if ev == PowerEvent::Resume {
        return LightAction::Reapply { after_ms: RESUME_DELAY_MS };
    }
    match level(s, state) {
        after if after == before => LightAction::Nothing,
        0 => LightAction::Dark,
        _ if before > 0 && matches!(ev, PowerEvent::Daylight { .. }) => LightAction::Level,
        _ => LightAction::Reapply { after_ms: 0 },
    }
}

/// `WM_POWERBROADCAST` wParam values (winuser.h).
pub const PBT_APMSUSPEND: u32 = 0x0004;
pub const PBT_APMRESUMEAUTOMATIC: u32 = 0x0012;

/// A `WM_POWERBROADCAST` without settings data.
pub fn broadcast_event(pbt: u32) -> Option<PowerEvent> {
    match pbt {
        PBT_APMSUSPEND => Some(PowerEvent::Suspend),
        PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resume),
        _ => None,
    }
}

/// The power settings the app registers for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSetting {
    /// GUID_LIDSWITCH_STATE_CHANGE: 0 closed, 1 open.
    Lid,
    /// GUID_CONSOLE_DISPLAY_STATE: 0 off, 1 on, 2 dimmed (counts as on).
    Display,
    /// GUID_ENERGY_SAVER_STATUS: 0 off, 1 standard, 2 high savings.
    EnergySaver,
}

/// A `PBT_POWERSETTINGCHANGE` with its DWORD value.
pub fn setting_event(setting: PowerSetting, value: u32) -> PowerEvent {
    match setting {
        PowerSetting::Lid => PowerEvent::Lid { open: value != 0 },
        PowerSetting::Display => PowerEvent::Display { on: value != 0 },
        PowerSetting::EnergySaver => PowerEvent::EnergySaver { on: value != 0 },
    }
}

// ---------------------------------------------------------------------------------------------------------
// Status (what the app's lighting page and `cockpit lighting status` show).

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyInfo {
    pub id: u8,
    pub name: &'static str,
    pub row: u8,
    /// x position in key units (fractional: 1.25), rows 16 units wide.
    pub col: f64,
    /// Width in key units (1 = a normal key).
    pub width: f64,
    /// false: an id AWCC lists that has no key on this keyboard (row 255, col and width 0); hidden on the map.
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<&'static str>,
}

/// The 91 ids in rows for the keyboard map (84 present), positions in key units (the table's quarter
/// units / 4). A key in two groups (W) names the later one (WASD).
pub fn layout() -> Vec<KeyInfo> {
    x17::KEYS.iter().map(|k| KeyInfo { id: k.id, name: k.name, row: k.row, col: k.col as f64 / 4.0, width: k.width as f64 / 4.0, present: k.present, group: x17::group_of(k.id) }).collect()
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub keyboard: bool,
    pub zones: bool,
    /// Always false: the zone controller accepts reports but its lights don't light on this laptop.
    pub zones_visible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LastApply {
    pub ok: bool,
    /// ms since the Unix epoch.
    pub at: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LightingStatus {
    pub settings: LightingSettings,
    pub layout: Vec<KeyInfo>,
    pub groups: Vec<String>,
    pub awcc: Option<AwccLook>,
    pub device: DeviceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_apply: Option<LastApply>,
}

pub fn status(settings: &LightingSettings, awcc: Option<AwccLook>, device: DeviceStatus, last_apply: Option<LastApply>) -> LightingStatus {
    LightingStatus { settings: settings.clone(), layout: layout(), groups: group_names(), awcc, device: DeviceStatus { zones_visible: false, ..device }, last_apply }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------------------
// Writing.

/// Where writes go and AWCC's look comes from: the real devices ([`SystemIo`]) or a test's ([`FakeIo`]).
pub trait LightIo: Send {
    /// The keyboard (API v5), opened for writing.
    fn keyboard(&mut self) -> Result<Box<dyn LightDevice>, String>;
    /// The zone controller (API v4), if there is one.
    fn zones(&mut self) -> Result<Option<Box<dyn LightDevice>>, String>;
    /// Pauses AWCC's lighting programs.
    fn pauser(&self) -> Box<dyn Pauser>;
    /// AWCC's active keyboard look.
    fn awcc(&mut self) -> Result<Option<AwccLook>, String>;
    /// Which devices are there (read-only).
    fn detect(&mut self) -> DeviceStatus;
}

/// What a write did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Applied {
    /// Keyboard reports sent.
    pub keyboard: usize,
    /// Zone controller reports sent.
    pub zones: usize,
    /// AWCC programs paused around the write.
    pub paused: Vec<String>,
    pub notes: Vec<String>,
}

impl Applied {
    pub fn text(&self) -> String {
        let mut t = format!("{} report(s) sent to the keyboard", self.keyboard);
        if self.zones > 0 {
            t.push_str(&format!(", {} to the zone controller (not visible on this laptop)", self.zones));
        }
        if !self.paused.is_empty() {
            t.push_str(&format!("; AWCC paused around it ({})", self.paused.join(", ")));
        }
        for n in &self.notes {
            t.push_str(&format!("; {n}"));
        }
        t.push('.');
        t
    }
}

/// Sends `steps` to the keyboard, with AWCC's lighting programs paused around it when `pause` is given. The
/// pause guard resumes them when this returns, whether the write worked, failed or panicked. A pause that
/// fails outright stops the write before anything is sent.
pub fn apply(dev: &mut dyn LightDevice, steps: &[V5Step], pause: Option<&dyn Pauser>) -> Result<(RunLog, Vec<String>, Vec<String>), String> {
    check_steps(Api::V5, steps)?;
    let guard = pause.map(|p| p.pause().map_err(|e| format!("couldn't pause AWCC: {e}; nothing was sent"))).transpose()?;
    let log = run_steps(dev, Api::V5, steps)?;
    let (paused, notes) = guard.as_ref().map(|g| (g.paused.clone(), g.notes.clone())).unwrap_or_default();
    drop(guard);
    Ok((log, paused, notes))
}

/// Writes `s` as it should show in power state `p`: the keyboard, then any zone colours, inside one pause of
/// `pause` when given (the command line passes `io.pauser()` when `pauseAwcc`; the app holds a
/// [`PauseLease`] instead and passes `None`). Callers check the mode first. A zone controller that is
/// missing or fails is a note, never an error: the app never relies on it.
pub fn write(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, pause: Option<&dyn Pauser>) -> Result<Applied, String> {
    write_from(io, Shown::AWCC, s, p, pause)
}

/// [`write`] from what the keyboard showed before (`prev`), sending only what changed ([`plan_from`]).
pub fn write_from(io: &mut dyn LightIo, prev: Shown, s: &LightingSettings, p: &PowerState, pause: Option<&dyn Pauser>) -> Result<Applied, String> {
    s.validate()?;
    let kb = plan_from(prev, s, p)?;
    let zones = zone_plan(s, p)?;
    check_steps(Api::V5, &kb)?;
    check_steps(Api::V4, &zones)?;
    let mut dev = io.keyboard()?;
    let mut zdev = if zones.is_empty() { None } else { io.zones().unwrap_or(None) };
    let guard = pause.map(|p| p.pause().map_err(|e| format!("couldn't pause AWCC: {e}; nothing was sent"))).transpose()?;
    let mut out = Applied::default();
    if let Some(g) = &guard {
        out.paused = g.paused.clone();
        out.notes = g.notes.clone();
    }
    out.keyboard = run_steps(dev.as_mut(), Api::V5, &kb)?.sent;
    if !zones.is_empty() {
        match zdev.as_mut() {
            Some(z) => match run_steps(z.as_mut(), Api::V4, &zones) {
                Ok(l) => out.zones = l.sent,
                Err(e) => out.notes.push(format!("zone colours not sent: {e}")),
            },
            None => out.notes.push("no zone controller found; zone colours not sent".into()),
        }
    }
    drop(guard);
    Ok(out)
}

/// Writes AWCC's own look back to the keyboard (paused as in [`write`]). Returns the look written.
pub fn restore(io: &mut dyn LightIo, pause: Option<&dyn Pauser>) -> Result<(AwccLook, Applied), String> {
    restore_from(io, Shown::AWCC, pause)
}

/// [`restore`] from what the keyboard showed before (`prev`).
pub fn restore_from(io: &mut dyn LightIo, prev: Shown, pause: Option<&dyn Pauser>) -> Result<(AwccLook, Applied), String> {
    let look = io.awcc()?.ok_or("AWCC has no active keyboard preset to restore")?;
    let steps = restore_awcc_plan_from(prev, &look)?;
    let mut dev = io.keyboard()?;
    let (log, paused, notes) = apply(dev.as_mut(), &steps, pause)?;
    Ok((look, Applied { keyboard: log.sent, zones: 0, paused, notes }))
}

/// AWCC paused across a run of writes (the app's live preview sends one per colour change): the first write
/// suspends AWCC's lighting programs, and they resume [`PauseLease::HOLD_MS`] after the last write, driven by
/// one timer. It lives behind the app's one lighting mutex; times are ms on any steady clock. Dropping the
/// lease (or [`PauseLease::release`], on exit) resumes AWCC at once, also while unwinding from a panic.
#[derive(Debug, Default)]
pub struct PauseLease {
    guard: Option<alienfx::device::PauseGuard>,
    last_ms: u64,
    timer: bool,
}

impl PauseLease {
    /// How long AWCC stays paused after the last write.
    pub const HOLD_MS: u64 = 2_000;

    /// Before a write at `now`: pauses AWCC unless already paused, and marks the write. True when the caller
    /// must start the timer (none is running); the timer then calls [`PauseLease::tick`].
    pub fn acquire(&mut self, pauser: &dyn Pauser, now: u64) -> Result<bool, String> {
        if self.guard.is_none() {
            self.guard = Some(pauser.pause().map_err(|e| format!("couldn't pause AWCC: {e}; nothing was sent"))?);
        }
        self.last_ms = now;
        Ok(!std::mem::replace(&mut self.timer, true))
    }

    /// Marks a write that just finished at `now` (the hold counts from the end of the last write).
    pub fn touch(&mut self, now: u64) {
        self.last_ms = self.last_ms.max(now);
    }

    /// The timer at `now`: resumes AWCC once [`PauseLease::HOLD_MS`] have passed since the last write and
    /// returns `None` (the timer stops); otherwise how long to wait before the next tick.
    pub fn tick(&mut self, now: u64) -> Option<u64> {
        let due = self.last_ms + Self::HOLD_MS;
        if self.guard.is_some() && now < due {
            return Some(due - now);
        }
        self.guard = None;
        self.timer = false;
        None
    }

    /// Resumes AWCC now (app exit, or `pauseAwcc` turned off).
    pub fn release(&mut self) {
        self.guard = None;
    }

    pub fn held(&self) -> bool {
        self.guard.is_some()
    }

    /// What is paused.
    pub fn paused(&self) -> Vec<String> {
        self.guard.as_ref().map(|g| g.paused.clone()).unwrap_or_default()
    }
}

/// The reports of `steps` as hex lines, the non-report steps in brackets (dry runs).
pub fn describe<const N: usize>(steps: &[Step<N>]) -> Vec<String> {
    steps
        .iter()
        .map(|s| match s {
            Step::Write(r) => alienfx::device::hex(r.as_bytes()),
            Step::WaitReady => "(wait until ready)".into(),
            Step::WaitBusy => "(wait until busy)".into(),
            Step::ReadStatus => "(read status)".into(),
        })
        .collect()
}

/// The real devices: the keyboard and zone controller found by the read-only probe, AWCC paused with
/// `NtSuspendProcess`, AWCC's look from a copy of its database. Windows only; elsewhere every write is
/// refused.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemIo;

/// Why there is no device here.
pub const WINDOWS_ONLY: &str = "lighting writes are Windows only";

impl LightIo for SystemIo {
    fn keyboard(&mut self) -> Result<Box<dyn LightDevice>, String> {
        #[cfg(windows)]
        {
            Ok(Box::new(alienfx::device::HidDevice::open(alienfx::ApiVersion::V5)?))
        }
        #[cfg(not(windows))]
        {
            Err(WINDOWS_ONLY.into())
        }
    }

    fn zones(&mut self) -> Result<Option<Box<dyn LightDevice>>, String> {
        #[cfg(windows)]
        {
            if alienfx::device::HidDevice::find(alienfx::ApiVersion::V4)?.is_none() {
                return Ok(None);
            }
            Ok(Some(Box::new(alienfx::device::HidDevice::open(alienfx::ApiVersion::V4)?)))
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }

    fn pauser(&self) -> Box<dyn Pauser> {
        Box::new(alienfx::device::AwccPauser)
    }

    fn awcc(&mut self) -> Result<Option<AwccLook>, String> {
        #[cfg(windows)]
        {
            let devices = lighting::read(&lighting::awcc_db()).map_err(|e| format!("{e:#}"))?;
            Ok(awcc_keys(&devices))
        }
        #[cfg(not(windows))]
        {
            Err("AWCC's lighting is read on Windows only".into())
        }
    }

    fn detect(&mut self) -> DeviceStatus {
        #[cfg(windows)]
        {
            match alienfx::probe::probe() {
                Ok(r) => {
                    let has = |v| r.interfaces.iter().any(|i| i.api == Some(v) && i.error.is_none());
                    DeviceStatus { keyboard: has(alienfx::ApiVersion::V5), zones: has(alienfx::ApiVersion::V4), zones_visible: false, error: None }
                }
                Err(e) => DeviceStatus { error: Some(e), ..Default::default() },
            }
        }
        #[cfg(not(windows))]
        {
            DeviceStatus { error: Some(WINDOWS_ONLY.into()), ..Default::default() }
        }
    }
}

/// Test devices: a fake keyboard (and optionally zone controller), a counting pauser and a given AWCC
/// preset. Clone it to keep handles on what was sent.
#[derive(Debug, Clone, Default)]
pub struct FakeIo {
    pub keyboard: FakeDevice,
    pub zones: Option<FakeDevice>,
    pub pauser: FakePauser,
    /// What `lighting::read` would return (`cockpit --json lighting show`'s `devices`).
    pub awcc: Vec<lighting::Device>,
}

impl LightIo for FakeIo {
    fn keyboard(&mut self) -> Result<Box<dyn LightDevice>, String> {
        Ok(Box::new(self.keyboard.clone()))
    }
    fn zones(&mut self) -> Result<Option<Box<dyn LightDevice>>, String> {
        Ok(self.zones.clone().map(|z| Box::new(z) as Box<dyn LightDevice>))
    }
    fn pauser(&self) -> Box<dyn Pauser> {
        Box::new(self.pauser.clone())
    }
    fn awcc(&mut self) -> Result<Option<AwccLook>, String> {
        Ok(awcc_keys(&self.awcc))
    }
    fn detect(&mut self) -> DeviceStatus {
        DeviceStatus { keyboard: true, zones: self.zones.is_some(), zones_visible: false, error: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cockpit --json lighting show` on the owner's x17 R2, 2026-11-24: AWCC's "System Default" preset,
    /// 11 keyboard groups covering the 91 LEDs.
    pub const SHOW_JSON: &str = r##"{"devices":[{"device":"0x0406","groups":[],"power_states":["DC - Sleep","DC - Working","DC - Low Battery","AC - Sleep","AC - Fully Charge","AC - Charging","Spectrum"],"preset":"System Default"},{"device":"0x0406_AdvCherryMechanicalKB","groups":[{"brightness":100,"colors":["#FF00FF"],"effect":4,"leds":[0,13,14,15,31,32,33,35,19,46,47,59,65,66,73,82,105,113,115]},{"brightness":100,"colors":["#2AAAFF"],"effect":4,"leds":[1,2,3,4,5,6,7,8,9,10,11,12]},{"brightness":100,"colors":["#2AAAFF"],"effect":4,"leds":[42,44,45]},{"brightness":100,"colors":["#3500FF"],"effect":4,"leds":[43,62,63,64]},{"brightness":100,"colors":["#FFCDE9"],"effect":4,"leds":[133,134,112,92,94,114,91,70,71,72,74,90,111,69,89,68,88,67,48,49,50,51,52,53,55]},{"brightness":100,"colors":["#6BFFB9"],"effect":4,"leds":[101,103,104,100,81,83,84,85,86,87]},{"brightness":100,"colors":["#6022FF"],"effect":4,"leds":[135,17,18,16]},{"brightness":100,"colors":["#FF00FF"],"effect":4,"leds":[61]},{"brightness":100,"colors":["#FF0000"],"effect":4,"leds":[40]},{"brightness":100,"colors":["#FFED87"],"effect":4,"leds":[20,21,22,23,24,25,26,27,28,29,30]},{"brightness":100,"colors":["#0000FF"],"effect":4,"leds":[109]}],"power_states":[],"preset":"System Default"}]}"##;

    fn preset() -> Vec<lighting::Device> {
        let v: serde_json::Value = serde_json::from_str(SHOW_JSON).unwrap();
        serde_json::from_value(v["devices"].clone()).unwrap()
    }

    fn custom() -> LightingSettings {
        LightingSettings { mode: Mode::Custom, ..Default::default() }
    }

    fn reports<const N: usize>(steps: &[Step<N>]) -> Vec<Vec<u8>> {
        steps.iter().filter_map(|s| s.report().map(|r| r.as_bytes().to_vec())).collect()
    }

    #[test]
    fn defaults_and_round_trip() {
        let d = LightingSettings::default();
        assert_eq!(d.mode, Mode::Awcc, "AWCC's look stays until the person opts in");
        assert!(d.off_on_sleep && d.off_on_lid_close && d.off_on_display_off && d.pause_awcc && !d.dim_on_battery_saver);
        assert_eq!((d.keyboard.on, d.keyboard.brightness, d.keyboard.effect), (true, 100, KeyEffect::Static));
        assert!(d.validate().is_ok());
        let v = serde_json::to_value(&d).unwrap();
        for k in ["mode", "keyboard", "zones", "offOnSleep", "offOnLidClose", "offOnDisplayOff", "dimOnBatterySaver", "pauseAwcc"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["mode"], "awcc");
        assert_eq!(v["keyboard"]["effect"], "static");
        let mut s = custom();
        s.keyboard.effect = KeyEffect::DualWave;
        s.keyboard.keys.insert(42, "#00FF00".into());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!((v["mode"].as_str(), v["keyboard"]["effect"].as_str(), v["keyboard"]["keys"]["42"].as_str()), (Some("custom"), Some("dualWave"), Some("#00FF00")));
        assert_eq!(serde_json::from_value::<LightingSettings>(v).unwrap(), s);
        let partial: LightingSettings = serde_json::from_str(r##"{ "mode": "custom", "keyboard": { "color": "#112233" } }"##).unwrap();
        assert_eq!((partial.keyboard.color.as_str(), partial.keyboard.brightness, partial.off_on_sleep), ("#112233", 100, true), "missing fields take defaults");
        assert_eq!(KeyEffect::parse("dual-wave"), Some(KeyEffect::DualWave));
        assert_eq!(KeyEffect::parse("NightRider"), Some(KeyEffect::NightRider));
        assert_eq!(KeyEffect::parse("spectrum"), None);
    }

    #[test]
    fn validation_refuses_out_of_range_and_unknown() {
        let bad = |f: &dyn Fn(&mut LightingSettings)| {
            let mut s = LightingSettings::default();
            f(&mut s);
            s.validate().unwrap_err()
        };
        assert!(bad(&|s| s.keyboard.brightness = 101).contains("0–100"));
        assert!(bad(&|s| s.keyboard.tempo = 256).contains("0–255"));
        assert!(bad(&|s| s.keyboard.color = "red".into()).contains("#RRGGBB"));
        assert!(bad(&|s| s.keyboard.color = "#12345".into()).contains("#RRGGBB"));
        assert!(bad(&|s| s.keyboard.color2 = "#GG0000".into()).contains("second colour"));
        assert!(bad(&|s| { s.keyboard.keys.insert(34, "#FFFFFF".into()); }).contains("isn't a key"), "34 is not one of the 91");
        assert!(bad(&|s| { s.keyboard.keys.insert(300, "#FFFFFF".into()); }).contains("isn't a key"));
        assert!(bad(&|s| { s.keyboard.keys.insert(0, "#FFF".into()); }).contains("key 0"));
        assert!(bad(&|s| { s.keyboard.groups.insert("Arrows".into(), "#FFFFFF".into()); }).contains("isn't a key group"));
        assert!(bad(&|s| { s.keyboard.groups.insert("wasd".into(), "#FFFFFF".into()); }).contains("isn't a key group"), "names as AWCC spells them");
        assert!(bad(&|s| { s.zones.insert(20, "#FFFFFF".into()); }).contains("0–19"));
        let mut ok = LightingSettings::default();
        ok.keyboard.brightness = 0;
        ok.keyboard.tempo = 255;
        ok.keyboard.keys.insert(135, "#abcdef".into());
        ok.keyboard.groups.insert("WASD".into(), "#FF0000".into());
        ok.zones.insert(19, "#000000".into());
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn composition_key_then_group_then_base() {
        let mut s = custom();
        s.keyboard.color = "#010101".into();
        s.keyboard.groups.insert("QWER".into(), "#020202".into());
        s.keyboard.groups.insert("WASD".into(), "#030303".into());
        s.keyboard.keys.insert(44, "#040404".into()); // E, in QWER
        s.keyboard.keys.insert(0, "#050505".into()); // ESC, in no group
        let c: BTreeMap<u8, Rgb> = compose(&s).into_iter().collect();
        assert_eq!(c.len(), 91);
        let g = |n| Rgb::new(n, n, n);
        assert_eq!(c[&44], g(4), "the key's own colour beats its group");
        assert_eq!(c[&0], g(5));
        assert_eq!(c[&42], g(2), "Q: QWER");
        assert_eq!(c[&43], g(3), "W is in QWER and WASD: the later group");
        assert_eq!(c[&62], g(3), "A: WASD");
        assert_eq!(c[&1], g(1), "F1: no Function Keys colour, so the base");
        assert_eq!(c[&135], g(1));
        assert_eq!(compose(&s).iter().map(|(id, _)| *id).collect::<Vec<_>>(), x17::KEYS.iter().map(|k| k.id).collect::<Vec<_>>(), "the layout's order");
    }

    #[test]
    fn brightness_scaling_and_power_levels() {
        assert_eq!((brightness_byte(0), brightness_byte(30), brightness_byte(50), brightness_byte(100)), (0, 77, 128, 255));
        let mut s = custom();
        s.keyboard.brightness = 80;
        let p = PowerState::default();
        assert_eq!(level(&s, &p), 80);
        assert_eq!(level(&s, &PowerState { energy_saver: true, ..p }), 80, "dimming is off by default");
        s.dim_on_battery_saver = true;
        assert_eq!(level(&s, &PowerState { energy_saver: true, ..p }), DIM_PCT);
        assert_eq!(level(&s, &PowerState { lid_closed: true, ..p }), 0);
        s.off_on_lid_close = false;
        assert_eq!(level(&s, &PowerState { lid_closed: true, ..p }), 80, "lid rule off");
        s.keyboard.on = false;
        assert_eq!(level(&s, &p), 0);
    }

    /// Each plan is exactly the alienfx builders' sequence, and reaches a fake device byte for byte.
    #[test]
    fn plans_match_the_builders_byte_for_byte() {
        let mut s = custom();
        s.keyboard.brightness = 50;
        let p = PowerState::default();
        let keys: Vec<Key> = compose(&s).into_iter().map(|(id, c)| Key::new(id, c)).collect();
        let mut want = v5::apply_keys(&keys).unwrap();
        want.extend(v5::brightness(128));
        assert_eq!(plan(&s, &p).unwrap(), want, "static at 50 %: no \"effect off\" coming from AWCC's per-key state");

        s.keyboard.effect = KeyEffect::MixPulse;
        s.keyboard.tempo = 200;
        s.keyboard.color = "#FF0000".into();
        s.keyboard.color2 = "#0000FF".into();
        let mut want = v5::global_effect(GlobalEffect::Effect { kind: EffectKind::MixPulse, tempo: 200, colors: ColorCount::Two, color1: Rgb::new(255, 0, 0), color2: Rgb::new(0, 0, 255) });
        want.extend(v5::brightness(128));
        assert_eq!(plan(&s, &p).unwrap(), want, "an effect");
        s.keyboard.effect = KeyEffect::Breathing;
        assert!(matches!(&plan(&s, &p).unwrap()[3], Step::Write(r) if r.as_bytes()[2] == EffectKind::Breathing as u8 && r.as_bytes()[9] == 0), "one colour");

        s.keyboard.on = false;
        assert_eq!(plan(&s, &p).unwrap(), v5::brightness(0), "off: brightness 0 only, colours untouched");
        s.keyboard.on = true;
        assert_eq!(plan(&s, &PowerState { suspended: true, ..p }).unwrap(), v5::brightness(0), "a dark trigger");

        // Through the fake device.
        s.keyboard.effect = KeyEffect::Static;
        let steps = plan(&s, &p).unwrap();
        let dev = FakeDevice::new();
        let (log, _, _) = apply(&mut dev.clone(), &steps, None).unwrap();
        assert_eq!(dev.sent(), reports(&steps));
        assert_eq!(log.sent, reports(&steps).len());
        assert!(dev.sent().iter().all(|r| alienfx::allowed(r, Api::V5)));
        assert_eq!(dev.sent().iter().filter(|r| r[1] == 0x8c && r[2] == 0x02).count(), 7, "91 keys in 7 colorSet reports of up to 15");
    }

    #[test]
    fn zone_plan_is_live_colours_only() {
        let mut s = custom();
        assert!(zone_plan(&s, &PowerState::default()).unwrap().is_empty());
        s.zones.insert(1, "#FF0000".into());
        let z = zone_plan(&s, &PowerState::default()).unwrap();
        let mut want = v4::reset();
        want.push(Step::Write(v4::set_one_color(1, Rgb::new(255, 0, 0))));
        want.push(Step::Write(v4::update()));
        assert_eq!(z, want);
        assert!(z.iter().filter_map(|s| s.report()).all(|r| !alienfx::is_memory_write(r.as_bytes(), Api::V4)));
        assert!(zone_plan(&s, &PowerState { display_off: true, ..Default::default() }).unwrap().is_empty());
    }

    #[test]
    fn write_pauses_awcc_and_resumes_on_error() {
        let mut s = custom();
        s.zones.insert(1, "#00FF00".into());
        let io = FakeIo { zones: Some(FakeDevice::new()), ..Default::default() };
        let a = write(&mut io.clone(), &s, &PowerState::default(), Some(&io.pauser)).unwrap();
        assert_eq!((io.pauser.paused(), io.pauser.resumed()), (1, 1), "one pause around keyboard and zones");
        assert_eq!(a.keyboard, io.keyboard.sent().len());
        assert_eq!(a.zones, 4);
        assert_eq!(io.keyboard.sent(), reports(&plan(&s, &PowerState::default()).unwrap()));

        io.keyboard.fail_at(3);
        let e = write(&mut io.clone(), &s, &PowerState::default(), Some(&io.pauser)).unwrap_err();
        assert!(e.contains("failed on demand"), "{e}");
        assert_eq!((io.pauser.paused(), io.pauser.resumed()), (2, 2), "AWCC resumed after the failed write");

        let dev = FakeDevice::new();
        dev.fail_at(0);
        let p = FakePauser::default();
        assert!(apply(&mut dev.clone(), &v5::brightness(0), Some(&p)).is_err());
        assert_eq!((p.paused(), p.resumed()), (1, 1), "apply resumes on error too");

        let p2 = p.clone();
        let r = std::panic::catch_unwind(move || {
            struct Panics;
            impl LightDevice for Panics {
                fn send_feature(&mut self, _: &[u8]) -> Result<(), String> {
                    panic!("device panicked")
                }
                fn get_feature(&mut self, _: &mut [u8]) -> Result<(), String> {
                    Ok(())
                }
                fn send_output(&mut self, _: &[u8]) -> Result<(), String> {
                    Ok(())
                }
                fn get_input(&mut self, _: &mut [u8]) -> Result<(), String> {
                    Ok(())
                }
            }
            let _ = apply(&mut Panics, &v5::brightness(0), Some(&p2));
        });
        assert!(r.is_err());
        assert_eq!((p.paused(), p.resumed()), (2, 2), "and on a panic");

        let io2 = FakeIo::default();
        write(&mut io2.clone(), &s, &PowerState::default(), None).unwrap();
        assert_eq!(io2.pauser.paused(), 0, "no pause given");

        io2.pauser.fail();
        let before = io2.keyboard.sent().len();
        assert!(write(&mut io2.clone(), &s, &PowerState::default(), Some(&io2.pauser)).unwrap_err().contains("nothing was sent"));
        assert_eq!(io2.keyboard.sent().len(), before);

        let mut bad = s.clone();
        bad.keyboard.brightness = 120;
        assert!(write(&mut io2.clone(), &bad, &PowerState::default(), None).is_err(), "validated before anything is sent");
        assert_eq!(io2.keyboard.sent().len(), before);
    }

    #[test]
    fn a_missing_zone_controller_is_a_note() {
        let mut s = custom();
        s.zones.insert(3, "#00FF00".into());
        let a = write(&mut FakeIo::default(), &s, &PowerState::default(), None).unwrap();
        assert!(a.notes.iter().any(|n| n.contains("no zone controller")), "{:?}", a.notes);
        assert_eq!(a.zones, 0);
    }

    #[test]
    fn awccs_look_per_key_and_restore_writes_all_91() {
        let look = awcc_keys(&preset()).unwrap();
        assert_eq!(look.preset, "System Default");
        assert_eq!(look.keys.len(), 91, "11 groups cover the 91 keys");
        assert_eq!(look.keys[&40], "#FF0000", "Tab");
        assert_eq!(look.keys[&43], "#3500FF", "W");
        assert_eq!(look.keys[&109], "#0000FF");
        assert_eq!(look.brightness, 100);
        assert!(awcc_keys(&preset()[..1]).is_none(), "the zone device has no groups");

        let steps = restore_awcc_plan(&look).unwrap();
        let io = FakeIo { awcc: preset(), ..Default::default() };
        let (l, a) = restore(&mut io.clone(), Some(&io.pauser)).unwrap();
        assert_eq!(l, look);
        assert_eq!(io.keyboard.sent(), reports(&steps));
        assert_eq!((io.pauser.paused(), io.pauser.resumed(), a.keyboard), (1, 1, reports(&steps).len()));
        // Every key's block {id + 1, r, g, b}, decoded back from the colorSet reports.
        let mut written: BTreeMap<u32, String> = BTreeMap::new();
        for r in io.keyboard.sent().iter().filter(|r| r[1] == 0x8c && r[2] == 0x02) {
            for b in r[4..].chunks(4).take_while(|b| b[0] != 0) {
                written.insert(b[0] as u32 - 1, rgb_hex(Rgb::new(b[1], b[2], b[3])));
            }
        }
        assert_eq!(written, look.keys, "all 91 keys in AWCC's colours");
        assert_eq!(io.keyboard.sent().last().unwrap()[1..3], [0x8b, 0x01], "from AWCC's own state: the proven sequence ends with update, no brightness report");
        assert!(restore(&mut FakeIo::default(), None).unwrap_err().contains("no active keyboard preset"));
        // From a dimmed effect: "effect off" first, brightness back to AWCC's 100 % last.
        let from_effect = restore_awcc_plan_from(Shown { effect: true, level: 40 }, &look).unwrap();
        let off = v5::global_effect(GlobalEffect::Off);
        assert_eq!(from_effect[..off.len()], off[..]);
        assert!(matches!(from_effect.last(), Some(Step::Write(r)) if r.as_bytes()[1..5] == [0x83, 0x38, 0x9c, 255]));
    }

    /// The LED balance measured on the owner's keyboard: white goes out as #8099FF (neutral there), a pure
    /// colour keeps full brightness, peach and lavender are rebalanced, and 100/100/100 changes nothing.
    #[test]
    fn the_led_balance_corrects_colours_and_keeps_brightness() {
        let b = LedBalance::OWNER_X17;
        assert_eq!(b.apply(Rgb::new(255, 255, 255)), Rgb::new(0x80, 0x99, 0xFF), "white: the neutral picked on the keyboard");
        assert_eq!(b.apply(Rgb::new(255, 0, 0)), Rgb::new(255, 0, 0), "pure red keeps full brightness");
        assert_eq!(b.apply(Rgb::BLACK), Rgb::BLACK);
        let peach = b.apply(Rgb::new(0xFF, 0x94, 0x80));
        assert_eq!(peach.r.max(peach.g).max(peach.b), 0xFF, "brightness kept");
        assert!(peach.b > peach.g, "blue up relative to green: {peach:?}");
        assert_eq!(LedBalance::default().apply(Rgb::new(0xFF, 0x94, 0x80)), Rgb::new(0xFF, 0x94, 0x80), "100/100/100 sends colours as they are");

        let mut s = custom();
        s.led_balance = b;
        s.keyboard.color = "#FFFFFF".into();
        s.keyboard.keys.clear();
        let sent: Vec<u8> = plan(&s, &PowerState::default()).unwrap().iter().filter_map(|x| x.report()).filter(|r| r.as_bytes()[1] == 0x8c && r.as_bytes()[2] == 0x02).flat_map(|r| r.as_bytes()[4..8].to_vec()).collect();
        assert_eq!(sent[1..4], [0x80, 0x99, 0xFF], "the plan sends balanced colours");
        let mut bad = s.clone();
        bad.led_balance.g = 5;
        assert!(bad.validate().unwrap_err().contains("LED balance green"));

        // A colour matched by eye wins over the balance; matching ignores case; others still balance.
        s.led_palette.insert("#ff9480".into(), "#FF5030".into());
        assert_eq!(s.led(Rgb::new(0xFF, 0x94, 0x80)), Rgb::new(0xFF, 0x50, 0x30));
        assert_eq!(s.led(Rgb::new(255, 255, 255)), Rgb::new(0x80, 0x99, 0xFF), "not in the palette: balanced");
        let mut bad = s.clone();
        bad.led_palette.insert("peach".into(), "#FF5030".into());
        assert!(bad.validate().unwrap_err().contains("LED palette"));
    }

    /// Only what changes is sent. From AWCC's own state (per-key, 100 %) plain colours are exactly the sequence
    /// proven on the owner's keyboard: reset, status, colour sets, loop, update. The first on-device run, which
    /// always added "effect off" and a brightness report, shifted reds, pinks and lightness.
    #[test]
    fn writes_send_only_what_changed() {
        let s = custom();
        let p = PowerState::default();
        let keys: Vec<Key> = compose(&s).into_iter().map(|(id, c)| Key::new(id, c)).collect();
        let proven = v5::apply_keys(&keys).unwrap();
        assert_eq!(plan_from(Shown::AWCC, &s, &p).unwrap(), proven, "static at 100 % from AWCC's state");
        let opcodes = |steps: &[V5Step]| steps.iter().filter_map(|x| x.report()).map(|r| r.as_bytes()[1]).collect::<Vec<_>>();
        assert!(!opcodes(&proven).contains(&0x80) && !opcodes(&proven).contains(&0x83), "no effect or brightness report");

        let from_effect = plan_from(Shown { effect: true, level: 100 }, &s, &p).unwrap();
        let off = v5::global_effect(GlobalEffect::Off);
        assert_eq!(from_effect[..off.len()], off[..], "leaving an effect: effect off first");
        assert_eq!(from_effect[off.len()..], proven[..]);

        let from_dark = plan_from(Shown { effect: false, level: 0 }, &s, &p).unwrap();
        assert_eq!(from_dark[..proven.len()], proven[..]);
        assert_eq!(from_dark[proven.len()..], v5::brightness(255)[..], "back from dark: full brightness again");

        let mut dim = s.clone();
        dim.keyboard.brightness = 30;
        assert_eq!(plan_from(Shown::AWCC, &dim, &p).unwrap().last(), v5::brightness(77).last(), "dimming sends brightness");
        assert_eq!(Shown::of(&dim, &p), Shown { effect: false, level: 30 });
        let mut fx = s.clone();
        fx.keyboard.effect = KeyEffect::Breathing;
        assert_eq!(Shown::of(&fx, &p), Shown { effect: true, level: 100 });
        assert_eq!(Shown::of(&fx, &PowerState { suspended: true, ..p }), Shown { effect: false, level: 0 }, "dark: neither effect nor light");
    }

    #[test]
    fn import_seeds_custom_settings_per_key() {
        let look = awcc_keys(&preset()).unwrap();
        let s = import_awcc(&custom(), &look);
        assert!(s.validate().is_ok());
        assert_eq!(s.keyboard.color, "#FFCDE9", "the most common colour (25 keys) is the base");
        assert_eq!(s.keyboard.keys.len(), 91 - 25);
        assert!(s.keyboard.groups.is_empty());
        let shown: BTreeMap<u32, String> = compose(&s).into_iter().map(|(id, c)| (id as u32, rgb_hex(c))).collect();
        assert_eq!(shown, look.keys, "composes back to exactly AWCC's look");
        assert_eq!(import_awcc(&LightingSettings::default(), &look).mode, Mode::Awcc, "the mode is kept: importing writes nothing");
    }

    #[test]
    fn power_events_drive_the_dark_and_restore_rules() {
        let s = custom();
        let mut st = PowerState::default();
        assert_eq!(on_power_event(&mut st, PowerEvent::Suspend, &s), LightAction::Dark);
        assert!(st.suspended);
        assert_eq!(on_power_event(&mut st, PowerEvent::Resume, &s), LightAction::Reapply { after_ms: RESUME_DELAY_MS });
        assert_eq!(on_power_event(&mut st, PowerEvent::Lid { open: false }, &s), LightAction::Dark);
        assert_eq!(on_power_event(&mut st, PowerEvent::Display { on: false }, &s), LightAction::Nothing, "already dark");
        assert_eq!(on_power_event(&mut st, PowerEvent::Lid { open: true }, &s), LightAction::Nothing, "still dark: the display is off");
        assert_eq!(on_power_event(&mut st, PowerEvent::Display { on: true }, &s), LightAction::Reapply { after_ms: 0 });
        assert_eq!(st, PowerState::default());
        assert_eq!(on_power_event(&mut st, PowerEvent::Display { on: true }, &s), LightAction::Nothing, "no change");
        assert_eq!(on_power_event(&mut st, PowerEvent::EnergySaver { on: true }, &s), LightAction::Nothing, "dimming is off by default");

        let mut dim = custom();
        dim.dim_on_battery_saver = true;
        let mut st = PowerState::default();
        assert_eq!(on_power_event(&mut st, PowerEvent::EnergySaver { on: true }, &dim), LightAction::Reapply { after_ms: 0 });
        assert_eq!(level(&dim, &st), DIM_PCT);
        assert_eq!(on_power_event(&mut st, PowerEvent::EnergySaver { on: false }, &dim), LightAction::Reapply { after_ms: 0 });

        let mut keep = custom();
        keep.off_on_sleep = false;
        keep.off_on_lid_close = false;
        let mut st = PowerState::default();
        assert_eq!(on_power_event(&mut st, PowerEvent::Suspend, &keep), LightAction::Nothing, "sleep rule off");
        assert_eq!(on_power_event(&mut st, PowerEvent::Lid { open: false }, &keep), LightAction::Nothing);
        assert_eq!(on_power_event(&mut st, PowerEvent::Resume, &keep), LightAction::Reapply { after_ms: RESUME_DELAY_MS }, "AWCC may have written meanwhile");

        let awcc = LightingSettings::default();
        let mut st = PowerState::default();
        for ev in [PowerEvent::Suspend, PowerEvent::Resume, PowerEvent::Lid { open: false }, PowerEvent::Display { on: false }, PowerEvent::EnergySaver { on: true }] {
            assert_eq!(on_power_event(&mut st, ev, &awcc), LightAction::Nothing, "awcc mode never writes: {ev:?}");
        }
        assert!(st.lid_closed && st.display_off && st.energy_saver, "the state is still tracked");

        assert_eq!(broadcast_event(PBT_APMSUSPEND), Some(PowerEvent::Suspend));
        assert_eq!(broadcast_event(PBT_APMRESUMEAUTOMATIC), Some(PowerEvent::Resume));
        assert_eq!(broadcast_event(0x0007), None, "PBT_APMRESUMESUSPEND follows the automatic one");
        assert_eq!(setting_event(PowerSetting::Lid, 0), PowerEvent::Lid { open: false });
        assert_eq!(setting_event(PowerSetting::Display, 2), PowerEvent::Display { on: true }, "dimmed counts as on");
        assert_eq!(setting_event(PowerSetting::EnergySaver, 2), PowerEvent::EnergySaver { on: true });
    }

    /// Day and night: the sun's level replaces the set brightness while it's on, under the dark rules and the
    /// Energy Saver cap; a change of level sends the brightness alone.
    #[test]
    fn day_and_night_sets_the_level_and_sends_brightness_only() {
        let mut s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        s.keyboard.brightness = 70;
        let sun = |pct| PowerState { daylight: Some(pct), ..Default::default() };
        assert_eq!(level(&s, &sun(40)), 70, "off: the set brightness");
        s.daylight.on = true;
        assert_eq!(level(&s, &PowerState::default()), 70, "on, but not looked yet: the set brightness");
        assert_eq!(level(&s, &sun(40)), 40);
        assert_eq!(level(&s, &PowerState { lid_closed: true, ..sun(40) }), 0, "dark rules still win");
        s.dim_on_battery_saver = true;
        assert_eq!(level(&s, &PowerState { energy_saver: true, ..sun(90) }), DIM_PCT, "Energy Saver still caps it");
        s.dim_on_battery_saver = false;

        let mut st = sun(100);
        assert_eq!(on_power_event(&mut st, PowerEvent::Daylight { pct: Some(100) }, &s), LightAction::Nothing, "same level");
        assert_eq!(on_power_event(&mut st, PowerEvent::Daylight { pct: Some(62) }, &s), LightAction::Level, "dusk: brightness only");
        assert_eq!(st.daylight, Some(62));
        assert_eq!(on_power_event(&mut st, PowerEvent::Daylight { pct: Some(0) }, &s), LightAction::Dark, "night at 0 %: off");
        assert_eq!(on_power_event(&mut st, PowerEvent::Daylight { pct: Some(10) }, &s), LightAction::Reapply { after_ms: 0 }, "back from dark: everything again");
        s.daylight.on = false;
        assert_eq!(on_power_event(&mut st, PowerEvent::Daylight { pct: None }, &s), LightAction::Nothing, "turned off: the save that turned it off wrote 70 % already");
        assert_eq!(level(&s, &st), 70);
        let awcc = LightingSettings { daylight: crate::daylight::DaylightSettings { on: true, ..Default::default() }, ..Default::default() };
        assert_eq!(on_power_event(&mut sun(100), PowerEvent::Daylight { pct: Some(20) }, &awcc), LightAction::Nothing, "awcc mode never writes");
        assert_eq!(level_plan(40), v5::brightness(102));
    }

    /// The preview's pause lease on a fake clock: one pause for a run of writes, AWCC resumed 2 s after the
    /// last one by the one timer, at once on release, and on drop.
    #[test]
    fn the_pause_lease_holds_across_writes_and_resumes_after_two_seconds() {
        let p = FakePauser::default();
        let mut lease = PauseLease::default();
        assert!(lease.acquire(&p, 0).unwrap(), "the first write starts the timer");
        assert!(!lease.acquire(&p, 300).unwrap(), "one timer");
        lease.touch(400);
        assert!(!lease.acquire(&p, 1_500).unwrap());
        lease.touch(1_600);
        assert_eq!((p.paused(), p.resumed()), (1, 0), "one pause for the run");
        assert_eq!(lease.tick(2_000), Some(1_600), "2 s after the last write ends");
        assert!(lease.held());
        assert_eq!(lease.tick(3_600), None, "resumed, the timer stops");
        assert_eq!((p.paused(), p.resumed()), (1, 1));
        assert!(!lease.held());
        assert!(lease.acquire(&p, 5_000).unwrap(), "a new run, a new timer");
        assert_eq!(p.paused(), 2);
        lease.release();
        assert_eq!(p.resumed(), 2, "exit resumes at once");
        assert_eq!(lease.tick(5_100), None, "the timer finds nothing held and stops");
        assert!(lease.acquire(&p, 6_000).unwrap());
        drop(lease);
        assert_eq!((p.paused(), p.resumed()), (3, 3), "drop resumes");
        let mut lease = PauseLease::default();
        p.fail();
        assert!(lease.acquire(&p, 0).is_err());
        assert!(!lease.held());
    }

    #[test]
    fn preview_is_one_key() {
        let p = preview_plan(42, "#123456").unwrap();
        assert_eq!(p, v5::apply_keys(&[Key::new(42, Rgb::new(0x12, 0x34, 0x56))]).unwrap());
        assert!(preview_plan(34, "#123456").is_err());
        assert!(preview_plan(42, "blue").is_err());
    }

    #[test]
    fn compose_and_restore_still_cover_all_91() {
        assert_eq!(compose(&custom()).len(), 91, "the 7 absent ids too, as AWCC writes them");
        let look = awcc_keys(&preset()).unwrap();
        let n: usize = restore_awcc_plan(&look).unwrap().iter().filter_map(|s| s.report()).filter(|r| r.as_bytes()[1] == 0x8c && r.as_bytes()[2] == 0x02).map(|r| r.as_bytes()[4..].chunks(4).take_while(|b| b[0] != 0).count()).sum();
        assert_eq!(n, 91);
    }

    #[test]
    fn status_shape() {
        let st = status(&custom(), awcc_keys(&preset()), DeviceStatus { keyboard: true, zones: true, zones_visible: true, error: None }, Some(LastApply { ok: true, at: 1, text: "x".into() }));
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["layout"].as_array().unwrap().len(), 91);
        assert_eq!(v["layout"][0], serde_json::json!({ "id": 0, "name": "ESC", "row": 0, "col": 0.0, "width": 1.0, "present": true }));
        assert_eq!(v["layout"][1]["group"], "Function Keys");
        let key = |id: u64| v["layout"].as_array().unwrap().iter().find(|k| k["id"] == id).unwrap().clone();
        assert_eq!((key(40)["width"].as_f64(), key(42)["col"].as_f64()), (Some(1.5), Some(1.5)), "Tab is 1.5 keys wide, Q at 1.5");
        assert_eq!((key(61)["width"].as_f64(), key(94)["col"].as_f64(), key(94)["width"].as_f64()), (Some(1.75), Some(12.25), Some(1.75)));
        assert_eq!((key(134)["name"].as_str(), key(134)["col"].as_f64(), key(114)["col"].as_f64()), (Some("Down / Page Down"), Some(14.0), Some(14.0)));
        assert_eq!(key(43)["group"], "WASD", "W names WASD");
        let present: Vec<&serde_json::Value> = v["layout"].as_array().unwrap().iter().filter(|k| k["present"] == true).collect();
        assert_eq!(present.len(), 84);
        assert_eq!((key(113)["present"].as_bool(), key(113)["row"].as_u64(), key(113)["width"].as_f64()), (Some(false), Some(255), Some(0.0)));
        for r in 0..6u64 {
            let used: f64 = present.iter().filter(|k| k["row"] == r).map(|k| k["width"].as_f64().unwrap()).sum();
            assert_eq!(used + if r == 5 { 5.0 } else { 0.0 }, 16.0, "row {r} is 16 units with the space bar");
        }
        assert_eq!(v["groups"], serde_json::json!(["Function Keys", "Numbers", "QWER", "WASD"]));
        assert_eq!(v["device"], serde_json::json!({ "keyboard": true, "zones": true, "zonesVisible": false }));
        assert_eq!(v["awcc"]["preset"], "System Default");
        assert_eq!(v["awcc"]["keys"]["40"], "#FF0000");
        assert!(v["awcc"].get("brightness").is_none());
        assert_eq!(v["lastApply"]["ok"], true);
        assert_eq!(v["settings"]["mode"], "custom");
        let none = serde_json::to_value(status(&LightingSettings::default(), None, DeviceStatus::default(), None)).unwrap();
        assert!(none["awcc"].is_null() && none.get("lastApply").is_none());
    }
}
