//! API v5 (the x17 R2 per-key keyboard, USB 0D62:CABC, collection FF89:00CC): 64-byte feature report 0xCC.
//!
//! Ported from alien-tools (MIT, https://github.com/T-Troll/alien-tools): the `COMMV5_*` templates in
//! alienrgb-controls.h 76-86 and the `API_V5` branches of `Functions` in AlienRGB_SDK.cpp.
#![forbid(unsafe_code)]

use crate::report::{Error, Report, Rgb, Step};

/// `length` for API v5 (`HIDP_CAPS::FeatureReportByteLength`, AlienRGB_SDK.cpp 198-200; 64 per
/// AlienRGB_SDK.h 103).
pub const REPORT_LEN: usize = 64;
/// `reportIDList[5]` (alienrgb-controls.h 14).
pub const REPORT_ID: u8 = 0xcc;
/// `brightnessScale[5]` (alienrgb-controls.h 15): v5 brightness runs 0..=255.
pub const BRIGHTNESS_SCALE: u8 = 0xff;

pub type V5Report = Report<REPORT_LEN>;
pub type V5Step = Step<REPORT_LEN>;

// Command opcodes (alienrgb-controls.h 76-83).
pub(crate) const OP_RESET: u8 = 0x94; // COMMV5_reset
pub(crate) const OP_STATUS: u8 = 0x93; // COMMV5_status
pub(crate) const OP_COLOR: u8 = 0x8c; // COMMV5_colorSet / COMMV5_loop
pub(crate) const OP_UPDATE: u8 = 0x8b; // COMMV5_update
pub(crate) const OP_TURN_ON: u8 = 0x83; // COMMV5_turnOnSet
pub(crate) const OP_EFFECT: u8 = 0x80; // COMMV5_setEffect

const COMM_RESET: [u8; 1] = [OP_RESET];
const COMM_STATUS: [u8; 1] = [OP_STATUS];
const COMM_COLOR_SET: [u8; 2] = [OP_COLOR, 0x02];
const COMM_LOOP: [u8; 2] = [OP_COLOR, 0x13];
const COMM_UPDATE: [u8; 3] = [OP_UPDATE, 0x01, 0xff];
const COMM_TURN_ON_SET: [u8; 3] = [OP_TURN_ON, 0x38, 0x9c];
pub(crate) const COMM_SET_EFFECT: [u8; 8] = [OP_EFFECT, 0x02, 0x07, 0x00, 0x00, 0x01, 0x01, 0x01];

/// First key block offset and stride in a colorSet report (AlienRGB_SDK.cpp 398: `bPos = 4; bPos += 4`).
const KEY_START: usize = 4;
const KEY_STRIDE: usize = 4;
/// Keys per colorSet report: offsets 4, 8, ..., 60 (`bPos < length` with length 64).
pub const KEYS_PER_REPORT: usize = (REPORT_LEN - KEY_START) / KEY_STRIDE;
/// Highest key id: the block carries `index + 1` (AlienRGB_SDK.cpp 372), so 255 would wrap to 0.
pub const MAX_KEY_ID: u8 = 254;

/// One key and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub id: u8,
    pub color: Rgb,
}

impl Key {
    pub const fn new(id: u8, color: Rgb) -> Self {
        Key { id, color }
    }
}

/// `COMMV5_reset` (alienrgb-controls.h 76): opens a colour set.
pub fn reset_report() -> V5Report {
    V5Report::from_command(REPORT_ID, &COMM_RESET).expect("template fits")
}

/// `COMMV5_status` (alienrgb-controls.h 77): asks the keyboard to put its status in the next feature read.
/// alien-tools always follows it with a get-feature read (`GetDeviceStatus`, AlienRGB_SDK.cpp 820-823).
pub fn status_request() -> V5Report {
    V5Report::from_command(REPORT_ID, &COMM_STATUS).expect("template fits")
}

/// `Reset()` for v5 (AlienRGB_SDK.cpp 308-312): reset, then `GetDeviceStatus()` (status request + read).
pub fn reset() -> Vec<V5Step> {
    vec![Step::Write(reset_report()), Step::Write(status_request()), Step::ReadStatus]
}

/// One `COMMV5_colorSet` report with 1..=[`KEYS_PER_REPORT`] keys: `AddV5DataBlock` (AlienRGB_SDK.cpp
/// 371-373) blocks `{index + 1, r, g, b}` from offset 4, as packed by `SetMultiAction`/`SetMultiColor`
/// (AlienRGB_SDK.cpp 396-404, 480-488). Refuses an empty list, too many keys and id 255.
pub fn color_set(keys: &[Key]) -> Result<V5Report, Error> {
    if keys.is_empty() {
        return Err(Error::Empty("keys"));
    }
    if keys.len() > KEYS_PER_REPORT {
        return Err(Error::TooMany { what: "keys", max: KEYS_PER_REPORT, got: keys.len() });
    }
    let mut r = V5Report::from_command(REPORT_ID, &COMM_COLOR_SET)?;
    for (i, k) in keys.iter().enumerate() {
        if k.id > MAX_KEY_ID {
            return Err(Error::OutOfRange { what: "v5 key id", value: k.id as u32, max: MAX_KEY_ID as u32 });
        }
        r.put(KEY_START + i * KEY_STRIDE, &[k.id + 1, k.color.r, k.color.g, k.color.b])?;
    }
    Ok(r)
}

/// `COMMV5_loop` (alienrgb-controls.h 80): ends the colour blocks of a set (AlienRGB_SDK.cpp 405, 489, 548).
pub fn loop_end() -> V5Report {
    V5Report::from_command(REPORT_ID, &COMM_LOOP).expect("template fits")
}

/// `COMMV5_update` (alienrgb-controls.h 81): applies the set (`UpdateColors`, AlienRGB_SDK.cpp 338-341).
pub fn update() -> V5Report {
    V5Report::from_command(REPORT_ID, &COMM_UPDATE).expect("template fits")
}

/// The colour blocks of a per-key set without reset or update: `SetMultiAction` v5 branch
/// (AlienRGB_SDK.cpp 480-490), colorSet reports of up to [`KEYS_PER_REPORT`] keys, then `loop`.
pub fn set_keys(keys: &[Key]) -> Result<Vec<V5Report>, Error> {
    if keys.is_empty() {
        return Err(Error::Empty("keys"));
    }
    let mut out = Vec::new();
    for chunk in keys.chunks(KEYS_PER_REPORT) {
        out.push(color_set(chunk)?);
    }
    out.push(loop_end());
    Ok(out)
}

/// One key, one colour: `SetAction` v5 branch (AlienRGB_SDK.cpp 545-548), a single block then `loop`.
pub fn set_key(id: u8, color: Rgb) -> Result<Vec<V5Report>, Error> {
    set_keys(&[Key::new(id, color)])
}

/// Several keys, one colour: `SetMultiColor` v5 branch (AlienRGB_SDK.cpp 396-406).
pub fn set_multi_color(ids: &[u8], color: Rgb) -> Result<Vec<V5Report>, Error> {
    let keys: Vec<Key> = ids.iter().map(|&id| Key::new(id, color)).collect();
    set_keys(&keys)
}

/// The full apply sequence for per-key colours, as alien-tools runs it from a closed set: `Reset()` (reset +
/// status read, AlienRGB_SDK.cpp 308-312), the colour blocks and `loop` ([`set_keys`]), then `UpdateColors()`
/// (update, AlienRGB_SDK.cpp 338-341).
pub fn apply_keys(keys: &[Key]) -> Result<Vec<V5Step>, Error> {
    let mut out = reset();
    out.extend(set_keys(keys)?.into_iter().map(Step::Write));
    out.push(Step::Write(update()));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------
// Brightness.

/// The brightness formula of `SetBrightness` for v5 (AlienRGB_SDK.cpp 738 with `brightnessScale[5]` 0xFF):
/// `((brightness * gbr) / 255) * 0xff / 0xff`, integer arithmetic.
pub const fn scale_brightness(brightness: u8, gbr: u8) -> u8 {
    let b = (brightness as u32 * gbr as u32) / 255;
    ((b * BRIGHTNESS_SCALE as u32) / 0xff) as u8
}

/// `COMMV5_turnOnSet` with `{4, {bright}}` (AlienRGB_SDK.cpp 745).
pub fn brightness_report(level: u8) -> V5Report {
    let mut r = V5Report::from_command(REPORT_ID, &COMM_TURN_ON_SET).expect("template fits");
    r.put(4, &[level]).expect("fixed offset");
    r
}

/// `SetBrightness` v5 branch (AlienRGB_SDK.cpp 743-746): `Reset()` then `turnOnSet`. alien-tools first calls
/// `UpdateColors()` when a set is open (line 736); that conditional report is left to the caller.
pub fn brightness(level: u8) -> Vec<V5Step> {
    let mut out = reset();
    out.push(Step::Write(brightness_report(level)));
    out
}

// ---------------------------------------------------------------------------------------------------------
// Global hardware effects.

/// Effect types for `COMMV5_setEffect` byte [2] (alienrgb-controls.h 85). Types 5 to 7 ("off?") are not
/// offered; type 0 / 1 are [`GlobalEffect::Off`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EffectKind {
    Breathing = 0x02,
    SingleColorWave = 0x03,
    DualColorWave = 0x04,
    Pulse = 0x08,
    MixPulse = 0x09,
    NightRider = 0x0a,
    Laser = 0x0b,
}

impl EffectKind {
    pub const ALL: [EffectKind; 7] = [
        EffectKind::Breathing,
        EffectKind::SingleColorWave,
        EffectKind::DualColorWave,
        EffectKind::Pulse,
        EffectKind::MixPulse,
        EffectKind::NightRider,
        EffectKind::Laser,
    ];
}

impl TryFrom<u8> for EffectKind {
    type Error = Error;
    fn try_from(v: u8) -> Result<Self, Error> {
        EffectKind::ALL
            .iter()
            .copied()
            .find(|k| *k as u8 == v)
            .ok_or(Error::OutOfRange { what: "v5 effect type", value: v as u32, max: 0x0b })
    }
}

/// `nc`, the number of colours of a global effect (AlienRGB_SDK.h 238: "3 - spectrum"). Sent as `nc - 1`
/// (AlienRGB_SDK.cpp 802); `nc` 0 would wrap to 0xFF, so it is not representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorCount {
    One = 1,
    Two = 2,
    Spectrum = 3,
}

impl TryFrom<u8> for ColorCount {
    type Error = Error;
    fn try_from(v: u8) -> Result<Self, Error> {
        match v {
            1 => Ok(ColorCount::One),
            2 => Ok(ColorCount::Two),
            3 => Ok(ColorCount::Spectrum),
            _ => Err(Error::OutOfRange { what: "v5 colour count", value: v as u32, max: 3 }),
        }
    }
}

/// A keyboard-wide hardware effect (`SetGlobalEffects`, AlienRGB_SDK.cpp 786-806).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalEffect {
    /// `effType == 0`: alien-tools sends type 1 ("reset") with 0xFE in the tempo byte, returning the keyboard
    /// to its per-key colours (AlienRGB_SDK.cpp 799-800).
    Off,
    Effect { kind: EffectKind, tempo: u8, colors: ColorCount, color1: Rgb, color2: Rgb },
}

/// One `COMMV5_setEffect` report (AlienRGB_SDK.cpp 799-803):
/// off: `{2, {1, 0xfe}}`; otherwise `{2, {effType, tempo}}, {9, {nc - 1, r1, g1, b1, r2, g2, b2}}`.
/// The `mode` argument of `SetGlobalEffects` is not sent for v5 (commented out in the C++).
pub fn effect_report(effect: GlobalEffect) -> V5Report {
    let mut r = V5Report::from_command(REPORT_ID, &COMM_SET_EFFECT).expect("template fits");
    match effect {
        GlobalEffect::Off => r.put(2, &[0x01, 0xfe]).expect("fixed offset"),
        GlobalEffect::Effect { kind, tempo, colors, color1, color2 } => {
            r.put(2, &[kind as u8, tempo]).expect("fixed offset");
            r.put(
                9,
                &[colors as u8 - 1, color1.r, color1.g, color1.b, color2.r, color2.g, color2.b],
            )
            .expect("fixed offset");
        }
    }
    r
}

/// `SetGlobalEffects` v5 branch (AlienRGB_SDK.cpp 795-804): `Reset()`, the effect report, then
/// `UpdateColors()`. alien-tools first calls `UpdateColors()` when a set is open (line 796-797); that
/// conditional report is left to the caller.
pub fn global_effect(effect: GlobalEffect) -> Vec<V5Step> {
    let mut out = reset();
    out.push(Step::Write(effect_report(effect)));
    out.push(Step::Write(update()));
    out
}

// ---------------------------------------------------------------------------------------------------------
// Status (pure parts only).

/// v5 status values (AlienRGB_SDK.h 25-27, `ALIENRGB_V5_*`).
pub const STATUS_START_COMMAND: u8 = 0x8c;
pub const STATUS_WAIT_UPDATE: u8 = 0x80;
pub const STATUS_IN_COMMAND: u8 = 0xcc;

/// The status byte of a v5 feature report: `buffer[2]` (`GetDeviceStatus`, AlienRGB_SDK.cpp 821-823).
pub fn status_byte(feature_report: &[u8]) -> Option<u8> {
    feature_report.get(2).copied()
}

/// `IsDeviceReady` v5 branch (AlienRGB_SDK.cpp 886-887): ready unless the status is WAITUPDATE (0x80).
pub const fn is_ready(status: u8) -> bool {
    status != STATUS_WAIT_UPDATE
}
