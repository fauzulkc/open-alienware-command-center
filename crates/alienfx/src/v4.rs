//! API v4 (the x17 R2 zone controller, USB 187C:0550): 34-byte output reports, report ID 0.
//!
//! Ported from alien-tools (MIT, https://github.com/T-Troll/alien-tools): the `COMMV4_*` templates and
//! `v4OpCodes` in alienrgb-controls.h 46-72, and the `API_V4` branches of `Functions` in AlienRGB_SDK.cpp.
#![forbid(unsafe_code)]

use crate::report::{Error, Report, Rgb, Step};

/// `length` for API v4 (`HIDP_CAPS::OutputReportByteLength`, AlienRGB_SDK.cpp 194, 211-212).
pub const REPORT_LEN: usize = 34;
/// `reportIDList[4]` (alienrgb-controls.h 14).
pub const REPORT_ID: u8 = 0x00;
/// `brightnessScale[4]` (alienrgb-controls.h 15): v4 brightness runs 0..=100.
pub const BRIGHTNESS_SCALE: u8 = 0x64;

pub type V4Report = Report<REPORT_LEN>;
pub type V4Step = Step<REPORT_LEN>;

// Command templates, without the leading length byte (alienrgb-controls.h 46-68).
pub(crate) const CMD: u8 = 0x03;
pub(crate) const OP_CONTROL: u8 = 0x21; // COMMV4_control
pub(crate) const OP_SET_POWER: u8 = 0x22; // COMMV4_setPower
pub(crate) const OP_COLOR_SEL: u8 = 0x23; // COMMV4_colorSel
pub(crate) const OP_COLOR_SET: u8 = 0x24; // COMMV4_colorSet
pub(crate) const OP_TURN_ON: u8 = 0x26; // COMMV4_turnOn
pub(crate) const OP_SET_ONE_COLOR: u8 = 0x27; // COMMV4_setOneColor

const COMM_CONTROL: [u8; 6] = [CMD, OP_CONTROL, 0x00, 0x03, 0xff, 0xff];
const COMM_COLOR_SEL: [u8; 5] = [CMD, OP_COLOR_SEL, 0x01, 0x00, 0x01];
const COMM_COLOR_SET: [u8; 7] = [CMD, OP_COLOR_SET, 0x00, 0x07, 0xd0, 0x00, 0xfa];
const COMM_SET_POWER: [u8; 2] = [CMD, OP_SET_POWER];
const COMM_TURN_ON: [u8; 2] = [CMD, OP_TURN_ON];
const COMM_SET_ONE_COLOR: [u8; 2] = [CMD, OP_SET_ONE_COLOR];

/// `v4OpCodes` (alienrgb-controls.h 72), indexed by [`Effect`].
pub const OP_CODES: [u8; 7] = [0xd0, 0xdc, 0xcf, 0xdc, 0x82, 0xac, 0xe8];

/// Light ids a v4 `setOneColor` report can carry: offsets 8..=33 (AlienRGB_SDK.cpp 411-414).
pub const MAX_COLOR_IDS: usize = 26;
/// Light ids a v4 `turnOn` (brightness) report can carry: offsets 6..=33 (AlienRGB_SDK.cpp 748-752).
pub const MAX_BRIGHTNESS_IDS: usize = 28;
/// Actions per `colorSet` report. See the note on [`color_set`].
pub const ACTIONS_PER_COLOR_SET: usize = 3;
/// Block stride in a `colorSet` report (AlienRGB_SDK.cpp 510: `bPos += 8`).
const ACTION_STRIDE: usize = 8;
/// First block offset in a `colorSet` report (AlienRGB_SDK.cpp 510: `bPos = 3`).
const ACTION_START: usize = 3;

/// The `Action` enum of AlienRGB_SDK.h 110-118 (`AlienFX_A_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Effect {
    Color = 0,
    Pulse = 1,
    Morph = 2,
    Breathing = 3,
    Spectrum = 4,
    Rainbow = 5,
    /// Power-button morph; only used inside the power-state slots.
    Power = 6,
}

impl Effect {
    pub const ALL: [Effect; 7] = [
        Effect::Color,
        Effect::Pulse,
        Effect::Morph,
        Effect::Breathing,
        Effect::Spectrum,
        Effect::Rainbow,
        Effect::Power,
    ];

    /// `v4OpCodes[type]` (alienrgb-controls.h 72).
    pub const fn op_code(self) -> u8 {
        OP_CODES[self as usize]
    }

    /// The action-type byte of a colorSet block: `type < Breathing ? type : Morph` (AlienRGB_SDK.cpp 512).
    pub const fn type_byte(self) -> u8 {
        if (self as u8) < (Effect::Breathing as u8) {
            self as u8
        } else {
            Effect::Morph as u8
        }
    }
}

impl TryFrom<u8> for Effect {
    type Error = Error;
    fn try_from(v: u8) -> Result<Self, Error> {
        Effect::ALL.get(v as usize).copied().ok_or(Error::OutOfRange { what: "v4 effect", value: v as u32, max: 6 })
    }
}

/// `Afx_action` (AlienRGB_SDK.h 85-90): one phase of a light's hardware effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Action {
    pub effect: Effect,
    /// How long the phase stays.
    pub time: u8,
    /// How fast it transforms. Ignored (sent as 0xFA, "steady") for [`Effect::Color`].
    pub tempo: u8,
    pub color: Rgb,
}

impl Action {
    pub const fn new(effect: Effect, time: u8, tempo: u8, color: Rgb) -> Self {
        Action { effect, time, tempo, color }
    }

    pub const fn color(color: Rgb) -> Self {
        Action::new(Effect::Color, 0, 0, color)
    }

    /// The 8-byte colorSet block (AlienRGB_SDK.cpp 511-513):
    /// `type, time, v4OpCodes[type], 0, tempo (0xFA for Color), r, g, b`.
    pub const fn block(&self) -> [u8; 8] {
        let tempo = match self.effect {
            Effect::Color => 0xfa,
            _ => self.tempo,
        };
        [
            self.effect.type_byte(),
            self.time,
            self.effect.op_code(),
            0,
            tempo,
            self.color.r,
            self.color.g,
            self.color.b,
        ]
    }
}

// ---------------------------------------------------------------------------------------------------------
// Control (0x21): start / finish / remove / play an action set.

/// `COMMV4_control` byte [4] (alienrgb-controls.h 47).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum ControlKind {
    StartNew = 1,
    FinishSave = 2,
    FinishPlay = 3,
    Remove = 4,
    Play = 5,
    SetDefault = 6,
}

/// Control target in bytes [5..=6]: 0xFFFF = the live (common) set, 0x0061 = the stored light set.
const TARGET_LIVE: [u8; 2] = [0xff, 0xff];
const TARGET_STORED_LIGHTS: [u8; 2] = [0x00, 0x61];

fn control(kind: ControlKind, target: [u8; 2]) -> V4Report {
    let mut r = V4Report::from_command(REPORT_ID, &COMM_CONTROL).expect("template fits");
    r.put(4, &[kind as u8]).expect("fixed offset");
    r.put(5, &target).expect("fixed offset");
    r
}

/// `Reset()` for v4 (AlienRGB_SDK.cpp 313-318): wait for ready, then control 4 (remove) and control 1
/// (start new) on the live set. alien-tools calls this before any colour command when no set is open.
pub fn reset() -> Vec<V4Step> {
    vec![
        Step::WaitReady,
        Step::Write(control(ControlKind::Remove, TARGET_LIVE)),
        Step::Write(control(ControlKind::StartNew, TARGET_LIVE)),
    ]
}

/// `UpdateColors()` for v4 (AlienRGB_SDK.cpp 342-345): the bare `COMMV4_control` template, control 3
/// (finish and play) on the live set. Applies what was sent since [`reset`].
pub fn update() -> V4Report {
    // The template already reads control 3 on 0xFFFF; building it through `control` keeps one code path.
    control(ControlKind::FinishPlay, TARGET_LIVE)
}

// ---------------------------------------------------------------------------------------------------------
// Colours and effects (live; nothing here is stored in the controller).

/// One light, one steady colour: `SetAction`, `AlienFX_A_Color` branch (AlienRGB_SDK.cpp 552-553),
/// `COMMV4_setOneColor` with `{3, {r, g, b, 0, 1, index}}`.
pub fn set_one_color(id: u8, color: Rgb) -> V4Report {
    let mut r = V4Report::from_command(REPORT_ID, &COMM_SET_ONE_COLOR).expect("template fits");
    r.put(3, &color.bytes()).expect("fixed offset");
    r.put(6, &[0, 1, id]).expect("fixed offset");
    r
}

/// Several lights, one steady colour: `SetMultiColor` v4 branch (AlienRGB_SDK.cpp 407-427).
/// `{3, {r, g, b, 0, count}}` then the ids from offset 8. Refuses an empty list and more than
/// [`MAX_COLOR_IDS`] ids (alien-tools splits longer lists with an update and reset in between; the x17 R2
/// zone controller has 12 lights, so that path is not ported).
pub fn set_multi_color(ids: &[u8], color: Rgb) -> Result<V4Report, Error> {
    if ids.is_empty() {
        return Err(Error::Empty("light ids"));
    }
    if ids.len() > MAX_COLOR_IDS {
        return Err(Error::TooMany { what: "light ids", max: MAX_COLOR_IDS, got: ids.len() });
    }
    let mut r = V4Report::from_command(REPORT_ID, &COMM_SET_ONE_COLOR)?;
    r.put(3, &color.bytes())?;
    r.put(6, &[0, ids.len() as u8])?;
    r.put(8, ids)?;
    Ok(r)
}

/// Select the light the following `colorSet` reports apply to: `SetV4Action` (AlienRGB_SDK.cpp 507),
/// `COMMV4_colorSel` with `{6, {index}}`.
pub fn color_select(id: u8) -> V4Report {
    let mut r = V4Report::from_command(REPORT_ID, &COMM_COLOR_SEL).expect("template fits");
    r.put(6, &[id]).expect("fixed offset");
    r
}

/// One `COMMV4_colorSet` report carrying 1..=3 action blocks (AlienRGB_SDK.cpp 510-516).
///
/// alien-tools loops `for (bPos = 3; bPos < length; bPos += 8)`, which with `length` 34 admits a fourth block
/// at offset 27 whose last byte (blue, offset 34) falls outside the 34-byte report and is never sent. This
/// crate stops at three blocks per report (as the header comment "up to 3" describes) and moves the fourth
/// action into the next report. For one to three actions the bytes are identical to alien-tools.
pub fn color_set(actions: &[Action]) -> Result<V4Report, Error> {
    if actions.is_empty() {
        return Err(Error::Empty("actions"));
    }
    if actions.len() > ACTIONS_PER_COLOR_SET {
        return Err(Error::TooMany { what: "actions", max: ACTIONS_PER_COLOR_SET, got: actions.len() });
    }
    let mut r = V4Report::from_command(REPORT_ID, &COMM_COLOR_SET)?;
    for (i, a) in actions.iter().enumerate() {
        r.put(ACTION_START + i * ACTION_STRIDE, &a.block())?;
    }
    Ok(r)
}

/// A hardware effect on one light: `SetV4Action` (AlienRGB_SDK.cpp 504-519). One `colorSel` for the light,
/// then `colorSet` reports of up to three actions each. Accepts every [`Effect`], including `Color` (which
/// `SaveLightsState` sends this way) and `Power` (used by the power slots).
pub fn v4_action(id: u8, actions: &[Action]) -> Result<Vec<V4Report>, Error> {
    if actions.is_empty() {
        return Err(Error::Empty("actions"));
    }
    let mut out = vec![color_select(id)];
    for chunk in actions.chunks(ACTIONS_PER_COLOR_SET) {
        out.push(color_set(chunk)?);
    }
    Ok(out)
}

/// `SetAction` for v4 (AlienRGB_SDK.cpp 521-524, 549-560): a first action of `Color` sends one
/// `setOneColor`; `Pulse`, `Morph`, `Breathing`, `Spectrum` and `Rainbow` go through [`v4_action`]. A first
/// action of `Power` is refused with [`Error::PowerAction`]: alien-tools sends it to `SetPowerAction`, which
/// writes the controller's memory (see [`set_power_action`]).
pub fn set_action(id: u8, actions: &[Action]) -> Result<Vec<V4Report>, Error> {
    let first = actions.first().ok_or(Error::Empty("actions"))?;
    match first.effect {
        Effect::Color => Ok(vec![set_one_color(id, first.color)]),
        Effect::Power => Err(Error::PowerAction),
        _ => v4_action(id, actions),
    }
}

/// Several lights with their own actions: `SetMultiAction` default branch (AlienRGB_SDK.cpp 491-496), one
/// [`set_action`] per light, without `save` (the `save` path is [`save_lights_state`]).
pub fn set_multi_action(lights: &[(u8, Vec<Action>)]) -> Result<Vec<V4Report>, Error> {
    if lights.is_empty() {
        return Err(Error::Empty("lights"));
    }
    let mut out = Vec::new();
    for (id, acts) in lights {
        out.extend(set_action(*id, acts)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------
// Brightness.

/// The brightness formula of `SetBrightness` (AlienRGB_SDK.cpp 738):
/// `((brightness * gbr) / 255) * brightnessScale[4] / 0xff`, integer arithmetic. `brightness` is 0 (off) to
/// 255 (full), `gbr` the device's global brightness from the mappings. Returns 0..=100.
pub const fn scale_brightness(brightness: u8, gbr: u8) -> u8 {
    let b = (brightness as u32 * gbr as u32) / 255;
    ((b * BRIGHTNESS_SCALE as u32) / 0xff) as u8
}

/// Hardware brightness: `SetBrightness` v4 branch (AlienRGB_SDK.cpp 747-766), `COMMV4_turnOn` with
/// `{3, {0x64 - bright, 0, count}}` and the ids from offset 6. `level` is the scaled brightness 0..=100 (see
/// [`scale_brightness`]); the byte sent is its inverse. Pass the ids alien-tools would include: every mapped
/// light whose flags are 0, plus power/indicator lights when `power` is set. Refuses `level > 100`, an empty
/// list and more than [`MAX_BRIGHTNESS_IDS`] ids.
pub fn brightness(level: u8, ids: &[u8]) -> Result<V4Report, Error> {
    if level > BRIGHTNESS_SCALE {
        return Err(Error::OutOfRange { what: "v4 brightness", value: level as u32, max: BRIGHTNESS_SCALE as u32 });
    }
    if ids.is_empty() {
        return Err(Error::Empty("light ids"));
    }
    if ids.len() > MAX_BRIGHTNESS_IDS {
        return Err(Error::TooMany { what: "light ids", max: MAX_BRIGHTNESS_IDS, got: ids.len() });
    }
    let mut r = V4Report::from_command(REPORT_ID, &COMM_TURN_ON)?;
    r.put(3, &[BRIGHTNESS_SCALE - level, 0, ids.len() as u8])?;
    r.put(6, ids)?;
    Ok(r)
}

// ---------------------------------------------------------------------------------------------------------
// Power-state slots and stored light state. These write the controller's memory.

/// The six power-state slots `SetPowerAction` programs (AlienRGB_SDK.cpp 655-685, `cid` 0x5B..0x60).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PowerSlot {
    AcSleep = 0x5b,
    AcPower = 0x5c,
    Charge = 0x5d,
    BatterySleep = 0x5e,
    BatteryPower = 0x5f,
    BatteryCritical = 0x60,
}

impl PowerSlot {
    pub const ALL: [PowerSlot; 6] = [
        PowerSlot::AcSleep,
        PowerSlot::AcPower,
        PowerSlot::Charge,
        PowerSlot::BatterySleep,
        PowerSlot::BatteryPower,
        PowerSlot::BatteryCritical,
    ];

    pub const fn id(self) -> u8 {
        self as u8
    }

    /// The action list `SetPowerAction` builds for this slot (AlienRGB_SDK.cpp 660-685). Note that alien-tools
    /// starts from a copy of the whole input block (`Afx_lightblock tact = *act;`) and appends to it, so the
    /// input actions come first; this is mirrored as written.
    pub fn actions(self, input: &[Action]) -> Result<Vec<Action>, Error> {
        let front = *input.first().ok_or(Error::Empty("power actions"))?;
        let back = *input.last().expect("non-empty");
        let off = Action::new(Effect::Power, 3, 0x64, Rgb::BLACK);
        let mut t = input.to_vec();
        match self {
            PowerSlot::AcSleep => {
                t.push(front);
                t.push(off);
            }
            PowerSlot::AcPower => {
                t.push(front);
                t[0].effect = Effect::Color;
            }
            PowerSlot::Charge => {
                t.push(front);
                t.push(back);
            }
            PowerSlot::BatterySleep => {
                t.push(back);
                t.push(off);
            }
            PowerSlot::BatteryPower => {
                t.push(back);
                t[0].effect = Effect::Color;
            }
            PowerSlot::BatteryCritical => {
                t.push(back);
                t[0].effect = Effect::Pulse;
            }
        }
        Ok(t)
    }
}

/// `COMMV4_setPower` with `{4, {kind, 0, cid}}` (AlienRGB_SDK.cpp 657-658, 688).
fn set_power(kind: ControlKind, slot: PowerSlot) -> V4Report {
    let mut r = V4Report::from_command(REPORT_ID, &COMM_SET_POWER).expect("template fits");
    r.put(4, &[kind as u8, 0, slot.id()]).expect("fixed offset");
    r
}

/// Opens a power-state slot for writing: `setPower` remove (4) then start (1) (AlienRGB_SDK.cpp 657-658).
///
/// **Writes the controller's memory.** Sending this erases the stored effect for `slot`. Needs the owner's
/// explicit go-ahead before any writer sends it.
pub fn power_slot_begin(slot: PowerSlot) -> [V4Report; 2] {
    [set_power(ControlKind::Remove, slot), set_power(ControlKind::StartNew, slot)]
}

/// Closes and saves a power-state slot: `setPower` finish-and-save (2) (AlienRGB_SDK.cpp 688).
///
/// **Writes the controller's memory.** Needs the owner's explicit go-ahead before any writer sends it.
pub fn power_slot_end(slot: PowerSlot) -> V4Report {
    set_power(ControlKind::FinishSave, slot)
}

/// Programs one power-state slot: begin, the slot's [`v4_action`] for light `id`, end
/// (AlienRGB_SDK.cpp 657-688, one loop iteration).
///
/// **Writes the controller's memory.** Needs the owner's explicit go-ahead before any writer sends it.
pub fn power_slot(slot: PowerSlot, id: u8, input: &[Action]) -> Result<Vec<V4Report>, Error> {
    let acts = slot.actions(input)?;
    let mut out = power_slot_begin(slot).to_vec();
    out.extend(v4_action(id, &acts)?);
    out.push(power_slot_end(slot));
    Ok(out)
}

/// The power-button sequence: `SetPowerAction` v4 branch (AlienRGB_SDK.cpp 652-699). All six slots 0x5B to
/// 0x60, then control 5 (play) on the live set, then wait for busy and ready. alien-tools first calls
/// `UpdateColors()` (line 649), which sends [`update`] only when a set is open; that conditional report is
/// left to the caller. `input` is the power light's action block (front = AC colour, back = battery colour).
///
/// **Writes the controller's memory.** This overwrites the stored power-button behaviour in every power
/// state. Needs the owner's explicit go-ahead before any writer sends it.
pub fn set_power_action(id: u8, input: &[Action]) -> Result<Vec<V4Step>, Error> {
    if input.is_empty() {
        return Err(Error::Empty("power actions"));
    }
    let mut out = Vec::new();
    for slot in PowerSlot::ALL {
        out.extend(power_slot(slot, id, input)?.into_iter().map(Step::Write));
    }
    out.push(Step::Write(control(ControlKind::Play, TARGET_LIVE)));
    out.push(Step::WaitBusy);
    out.push(Step::WaitReady);
    Ok(out)
}

/// Stores light effects as the power-on defaults: `SaveLightsState` v4 branch (AlienRGB_SDK.cpp 617-625).
/// Control 4 then 1 on target 0x61, a [`v4_action`] for each light whose first action is not `Power`,
/// then control 2 (finish and save) and 6 (set default) on 0x61. alien-tools first calls `UpdateColors()`
/// when a set is open (line 614-615); that conditional report is left to the caller.
///
/// **Writes the controller's memory.** This replaces the stored power-on lighting (slot 0x61). Needs the
/// owner's explicit go-ahead before any writer sends it.
pub fn save_lights_state(lights: &[(u8, Vec<Action>)]) -> Result<Vec<V4Report>, Error> {
    let mut out = vec![
        control(ControlKind::Remove, TARGET_STORED_LIGHTS),
        control(ControlKind::StartNew, TARGET_STORED_LIGHTS),
    ];
    for (id, acts) in lights {
        let first = acts.first().ok_or(Error::Empty("actions"))?;
        if first.effect != Effect::Power {
            out.extend(v4_action(*id, acts)?);
        }
    }
    out.push(control(ControlKind::FinishSave, TARGET_STORED_LIGHTS));
    out.push(control(ControlKind::SetDefault, TARGET_STORED_LIGHTS));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------
// Status (pure parts only: nothing here reads a device).

/// v4 status values (AlienRGB_SDK.h 19-23, `ALIENRGB_V4_*`).
pub const STATUS_READY: u8 = 33;
pub const STATUS_BUSY: u8 = 34;
pub const STATUS_WAIT_COLOR: u8 = 35;
pub const STATUS_WAIT_UPDATE: u8 = 36;
pub const STATUS_WAS_ON: u8 = 38;

/// `WaitForReady` / `WaitForBusy` poll interval (AlienRGB_SDK.cpp 854, 874: `Sleep(20)`).
pub const POLL_INTERVAL_MS: u32 = 20;
/// `WaitForBusy` poll limit (AlienRGB_SDK.cpp 874: `i < 500`).
pub const BUSY_POLL_LIMIT: u32 = 500;

/// A decoded v4 status byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// 0: no answer (alien-tools treats it as a hang).
    None,
    Ready,
    Busy,
    WaitColor,
    WaitUpdate,
    WasOn,
    Other(u8),
}

impl Status {
    pub const fn from_byte(b: u8) -> Status {
        match b {
            0 => Status::None,
            STATUS_READY => Status::Ready,
            STATUS_BUSY => Status::Busy,
            STATUS_WAIT_COLOR => Status::WaitColor,
            STATUS_WAIT_UPDATE => Status::WaitUpdate,
            STATUS_WAS_ON => Status::WasOn,
            other => Status::Other(other),
        }
    }
}

/// The status byte of a v4 input report: `buffer[2]` (`GetDeviceStatus`, AlienRGB_SDK.cpp 827-830).
/// `None` if the report is shorter than 3 bytes.
pub fn status_byte(input_report: &[u8]) -> Option<u8> {
    input_report.get(2).copied()
}

/// `IsDeviceReady` result for v4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    NotReady,
    /// Status 0: alien-tools returns 0xFF ("stalled") here, which its wait loop treats as ready.
    Stalled,
}

/// `IsDeviceReady` v4 branch (AlienRGB_SDK.cpp 888-895): `status ? status != BUSY : 0xff`.
pub const fn readiness(status: u8) -> Readiness {
    match status {
        0 => Readiness::Stalled,
        STATUS_BUSY => Readiness::NotReady,
        _ => Readiness::Ready,
    }
}

/// `WaitForBusy` v4 branch (AlienRGB_SDK.cpp 871-872): skipped for PID 0x0551 ("patch for newer v4"). The x17
/// R2's 0x0550 does wait.
pub const fn waits_for_busy(pid: u16) -> bool {
    pid != 0x0551
}
