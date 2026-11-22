//! Alienware AWCC firmware interface, as pure logic (docs/plans/x17-control-service.md §2-3).
//!
//! The firmware is reached through `root\WMI AWCCWmiMethodFunction` (instance `ACPI\PNP0C14\AWCC_0`), one
//! method call at a time with a packed `u32` passed as `arg2` and a `u32` answer `argr`. Dell doesn't
//! document it; the read side was verified on the owner's x17 R2 (BIOS 1.32.0) on 2026-10-07, and the two
//! write ops come from the owner's x17tune and the Linux `alienware-wmi-wmax` driver.
//!
//! Safety lives in the types: only the calls below can be built ([`Call`]), every argument is a `u8` (so an
//! out-of-range value can't be packed), a write is only possible for an id the firmware itself reported
//! ([`Discovery::write`]), and the service's string boundary is checked with [`allowed`].

pub mod rules;

use serde::Serialize;
use std::collections::BTreeMap;

/// The only firmware methods Wiregent ever calls. Overclocking, memory OC, GPIO, Caldera, Tobii and `BFn`
/// have no variant, so they can't be called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Method {
    ThermalInformation,
    GetFanSensors,
    ThermalControl,
}

impl Method {
    pub fn wmi_name(self) -> &'static str {
        match self {
            Method::ThermalInformation => "Thermal_Information",
            Method::GetFanSensors => "GetFanSensors",
            Method::ThermalControl => "Thermal_Control",
        }
    }
}

// Thermal_Information (read)
pub const OP_SYSTEM_DESCRIPTION: u8 = 0x02;
pub const OP_RESOURCE_ID: u8 = 0x03;
pub const OP_TEMPERATURE: u8 = 0x04;
pub const OP_FAN_RPM: u8 = 0x05;
pub const OP_FAN_MIN_RPM: u8 = 0x08;
pub const OP_FAN_MAX_RPM: u8 = 0x09;
pub const OP_CURRENT_PROFILE: u8 = 0x0B;
pub const OP_FAN_BOOST: u8 = 0x0C;
// GetFanSensors (read)
pub const OP_FAN_SENSOR_COUNT: u8 = 0x01;
pub const OP_FAN_SENSOR_ID: u8 = 0x02;
// Thermal_Control (write)
pub const OP_ACTIVATE_PROFILE: u8 = 0x01;
pub const OP_SET_FAN_BOOST: u8 = 0x02;

/// The firmware's "unsupported / error" answer.
pub const ERROR: u32 = 0xFFFF_FFFF;

/// A temperature answer (op 0x04) as °C, or None when it's an error or not plausible for a running laptop
/// (0 °C and anything above 125 °C are sensor faults, not readings).
pub fn decode_temp(argr: u32) -> Option<i32> {
    (argr != ERROR && (1..=125).contains(&argr)).then_some(argr as i32)
}

/// A fan-speed answer (op 0x05) as RPM, or None when it's an error or impossible. `max_rpm` is the fan's own
/// op 0x09 answer; fans do run above it (0x32 read 4600 against 4200 in G-Mode), so the limit is 1.5 × max,
/// or 10 000 when the fan reports no max.
pub fn decode_rpm(argr: u32, max_rpm: Option<u32>) -> Option<u32> {
    let limit = match max_rpm {
        Some(m) if m > 0 && m != ERROR => m.saturating_mul(3) / 2,
        _ => 10_000,
    };
    (argr != ERROR && argr <= limit).then_some(argr)
}

/// A profile answer (op 0x0B), or None for an error or a value outside one byte.
pub fn decode_profile(argr: u32) -> Option<u8> {
    (argr != ERROR && argr <= 0xFF).then_some(argr as u8)
}

/// The balanced profile the watchdog falls back to.
pub const BALANCED: u8 = 0xA0;
/// G-Mode. The Linux driver leaves it off on the x17, so writing it needs its own opt-in (H2, case C-24).
pub const GMODE: u8 = 0xAB;

/// `op | arg1 << 8 | arg2 << 16 | arg3 << 24`. Every part is a `u8`, so nothing can spill into the next byte.
pub const fn pack(op: u8, arg1: u8, arg2: u8, arg3: u8) -> u32 {
    op as u32 | (arg1 as u32) << 8 | (arg2 as u32) << 16 | (arg3 as u32) << 24
}

/// The method + op allow-list, for the one place a method name arrives as text (the service's WMI layer
/// re-checks every call it is about to make).
pub fn allowed(method: &str, op: u8) -> bool {
    match method {
        "Thermal_Information" => matches!(op, OP_SYSTEM_DESCRIPTION | OP_RESOURCE_ID | OP_TEMPERATURE | OP_FAN_RPM | OP_FAN_MIN_RPM | OP_FAN_MAX_RPM | OP_CURRENT_PROFILE | OP_FAN_BOOST),
        "GetFanSensors" => matches!(op, OP_FAN_SENSOR_COUNT | OP_FAN_SENSOR_ID),
        "Thermal_Control" => matches!(op, OP_ACTIVATE_PROFILE | OP_SET_FAN_BOOST),
        _ => false,
    }
}

/// One firmware call. Reads are built with the `read_*` functions; writes only through [`Discovery::write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Call {
    pub method: Method,
    pub op: u8,
    pub arg1: u8,
    pub arg2: u8,
}

impl Call {
    const fn read(method: Method, op: u8, arg1: u8, arg2: u8) -> Self {
        Call { method, op, arg1, arg2 }
    }
    pub fn packed(&self) -> u32 {
        pack(self.op, self.arg1, self.arg2, 0)
    }
    pub fn is_write(&self) -> bool {
        self.method == Method::ThermalControl
    }
}

pub const fn read_system_description() -> Call {
    Call::read(Method::ThermalInformation, OP_SYSTEM_DESCRIPTION, 0, 0)
}
pub const fn read_resource_id(index: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_RESOURCE_ID, index, 0)
}
pub const fn read_temperature(sensor: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_TEMPERATURE, sensor, 0)
}
pub const fn read_fan_rpm(fan: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_FAN_RPM, fan, 0)
}
pub const fn read_fan_min_rpm(fan: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_FAN_MIN_RPM, fan, 0)
}
pub const fn read_fan_max_rpm(fan: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_FAN_MAX_RPM, fan, 0)
}
pub const fn read_current_profile() -> Call {
    Call::read(Method::ThermalInformation, OP_CURRENT_PROFILE, 0, 0)
}
pub const fn read_fan_boost(fan: u8) -> Call {
    Call::read(Method::ThermalInformation, OP_FAN_BOOST, fan, 0)
}
pub const fn read_fan_sensor_count(fan: u8) -> Call {
    Call::read(Method::GetFanSensors, OP_FAN_SENSOR_COUNT, fan, 0)
}
pub const fn read_fan_sensor_id(fan: u8, index: u8) -> Call {
    Call::read(Method::GetFanSensors, OP_FAN_SENSOR_ID, fan, index)
}

/// What op 0x02 says the machine has: byte 0 fans, byte 1 sensors, byte 2 unknown, byte 3 profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SystemDescription {
    pub fans: u8,
    pub sensors: u8,
    pub unknown: u8,
    pub profiles: u8,
}

impl SystemDescription {
    pub fn decode(argr: u32) -> Option<Self> {
        if argr == ERROR {
            return None;
        }
        let b = argr.to_le_bytes();
        Some(SystemDescription { fans: b[0], sensors: b[1], unknown: b[2], profiles: b[3] })
    }
    pub fn total(&self) -> u16 {
        self.fans as u16 + self.sensors as u16 + self.unknown as u16 + self.profiles as u16
    }
}

/// A profile id's name as AWCC shows it (from AWCC's own thermal service data on this unit, 2026-11-09:
/// `OCControl.Service.Thermal.Data\Profile.json`). The Linux driver names some of these differently: its
/// "balanced-performance" (0xA1) is AWCC's Performance and its "performance" (0xA4) is AWCC's Full Speed.
pub fn profile_name(id: u8) -> &'static str {
    match id {
        0x00 => "Custom",
        0x96 => "legacy quiet",
        0x97 => "legacy balanced",
        0x98 => "legacy balanced-performance",
        0x99 => "legacy performance",
        0xA0 => "Balanced",
        0xA1 => "Performance",
        0xA2 => "Cool",
        0xA3 => "Quiet",
        0xA4 => "Full Speed",
        0xA5 => "Battery Saver",
        0xAB => "G-Mode",
        _ => "unknown",
    }
}

/// Why discovery or a write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The firmware didn't answer, or answered "unsupported", for this call.
    NoAnswer(Call),
    /// The answer doesn't fit what op 0x02 said (more resources than declared, a zero id).
    Inconsistent(String),
    UnknownProfile(u8),
    UnknownFan(u8),
    /// G-Mode is reported but not cleared for writes on this machine.
    GModeNotCleared,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoAnswer(c) => write!(f, "no answer from {} op 0x{:02X}", c.method.wmi_name(), c.op),
            Error::Inconsistent(s) => write!(f, "firmware answers don't agree: {s}"),
            Error::UnknownProfile(p) => write!(f, "profile 0x{p:02X} isn't one this machine reported"),
            Error::UnknownFan(x) => write!(f, "fan 0x{x:02X} isn't one this machine reported"),
            Error::GModeNotCleared => write!(f, "G-Mode isn't cleared for writes on this machine yet"),
        }
    }
}

impl std::error::Error for Error {}

/// What the firmware reported about itself. Ids come from the firmware, never from a table here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Discovery {
    pub description: SystemDescription,
    pub fans: Vec<u8>,
    pub sensors: Vec<u8>,
    pub profiles: Vec<u8>,
    /// Which temperature sensor each fan follows (GetFanSensors).
    pub fan_sensor: BTreeMap<u8, u8>,
}

impl Discovery {
    /// Runs discovery through `read`, which makes one firmware call (real WMI, or a fixture replay).
    pub fn run(mut read: impl FnMut(Call) -> Option<u32>) -> Result<Self, Error> {
        let mut ask = |c: Call| read(c).filter(|v| *v != ERROR).ok_or(Error::NoAnswer(c));
        let d = SystemDescription::decode(ask(read_system_description())?).ok_or(Error::NoAnswer(read_system_description()))?;
        if d.total() > 0xFF {
            return Err(Error::Inconsistent(format!("{} resources", d.total())));
        }
        let mut ids = Vec::with_capacity(d.total() as usize);
        for i in 0..d.total() as u8 {
            let id = (ask(read_resource_id(i))? & 0xFF) as u8;
            if id == 0 {
                return Err(Error::Inconsistent(format!("resource {i} has id 0")));
            }
            ids.push(id);
        }
        let (f, s, u) = (d.fans as usize, d.sensors as usize, d.unknown as usize);
        let fans = ids[..f].to_vec();
        let sensors = ids[f..f + s].to_vec();
        let profiles = ids[f + s + u..].to_vec();
        let mut fan_sensor = BTreeMap::new();
        for &fan in &fans {
            if ask(read_fan_sensor_count(fan))? >= 1 {
                let sensor = (ask(read_fan_sensor_id(fan, 0))? & 0xFF) as u8;
                if !sensors.contains(&sensor) {
                    return Err(Error::Inconsistent(format!("fan 0x{fan:02X} follows unknown sensor 0x{sensor:02X}")));
                }
                fan_sensor.insert(fan, sensor);
            }
        }
        Ok(Discovery { description: d, fans, sensors, profiles, fan_sensor })
    }

    /// The fans that follow the same sensor as `fan` (its partners).
    pub fn partners(&self, fan: u8) -> Vec<u8> {
        let Some(s) = self.fan_sensor.get(&fan) else { return Vec::new() };
        self.fan_sensor.iter().filter(|(f, t)| **f != fan && *t == s).map(|(f, _)| *f).collect()
    }

    /// The text lists the plugin host resolves enum arguments against (`profiles`, `fans`), as "0xA0".
    pub fn lists(&self) -> BTreeMap<String, Vec<String>> {
        let hex = |v: &[u8]| v.iter().map(|x| format!("0x{x:02X}")).collect();
        BTreeMap::from([("profiles".to_string(), hex(&self.profiles)), ("fans".to_string(), hex(&self.fans)), ("sensors".to_string(), hex(&self.sensors))])
    }

    /// The only way to build a write: the target must be one the firmware reported.
    pub fn write(&self, w: Write, gmode_cleared: bool) -> Result<Call, Error> {
        match w {
            Write::Profile(p) => {
                if !self.profiles.contains(&p) {
                    return Err(Error::UnknownProfile(p));
                }
                if p == GMODE && !gmode_cleared {
                    return Err(Error::GModeNotCleared);
                }
                Ok(Call { method: Method::ThermalControl, op: OP_ACTIVATE_PROFILE, arg1: p, arg2: 0 })
            }
            Write::FanBoost { fan, level } => {
                if !self.fans.contains(&fan) {
                    return Err(Error::UnknownFan(fan));
                }
                Ok(Call { method: Method::ThermalControl, op: OP_SET_FAN_BOOST, arg1: fan, arg2: level })
            }
        }
    }

    /// The watchdog's restore: balanced, then every reported fan's boost back to 0 (the firmware's own curve).
    pub fn restore(&self) -> Vec<Call> {
        let mut calls = Vec::new();
        if let Ok(c) = self.write(Write::Profile(BALANCED), false) {
            calls.push(c);
        }
        calls.extend(self.fans.iter().filter_map(|&fan| self.write(Write::FanBoost { fan, level: 0 }, false).ok()));
        calls
    }
}

/// A requested change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    Profile(u8),
    FanBoost { fan: u8, level: u8 },
}

/// Parses a capture made by `awcc_read.ps1` into `(method, packed, argr)` rows, for replay in tests and for
/// comparing the Rust probe with the PowerShell one (case P-05).
pub fn parse_capture(text: &str) -> Vec<(String, u32, u32)> {
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    text.lines()
        .filter_map(|line| {
            let method = line.split_whitespace().next()?;
            if !matches!(method, "Thermal_Information" | "GetFanSensors") {
                return None;
            }
            let packed = line.split("packed=").nth(1)?.split_whitespace().next()?;
            let argr = line.split("argr=").nth(1)?.split_whitespace().next()?;
            Some((method.to_string(), hex(packed)?, hex(argr)?))
        })
        .collect()
}

/// One row in the same text form `awcc_read.ps1` writes, so either probe's output parses with [`parse_capture`].
pub fn capture_line(c: &Call, argr: Option<u32>) -> String {
    let head = format!("{:<20} op=0x{:02X} arg1=0x{:02X} arg2=0x{:02X} packed=0x{:08X}", c.method.wmi_name(), c.op, c.arg1, c.arg2, c.packed());
    match argr {
        Some(v) => format!("{head} -> argr=0x{v:08X} ({v})"),
        None => format!("{head} -> FAILED"),
    }
}

/// The 2026-10-07 reference capture (F1), shipped with the probe so H0 can compare without the owner's repo.
pub const REFERENCE_CAPTURE: &str = include_str!("../tests/fixtures/f1-awcc-probe-2026-10-07.txt");

/// How one call compared between a new capture and the reference (case P-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Compared {
    pub method: String,
    pub packed: u32,
    pub reference: u32,
    pub now: Option<u32>,
    /// "exact" (static), "temp ±2", "rpm ±100", or "info" (expected to change: the current profile).
    pub rule: &'static str,
    pub ok: bool,
}

/// Compares a new capture with the reference: what the firmware *is* (description, ids, limits, fan→sensor)
/// must match exactly; live values within tolerance; the current profile is reported, never failed.
pub fn compare(reference: &[(String, u32, u32)], now: &[(String, u32, u32)]) -> Vec<Compared> {
    reference
        .iter()
        .map(|(method, packed, r)| {
            let n = now.iter().find(|(m, p, _)| m == method && p == packed).map(|x| x.2);
            let op = (*packed & 0xFF) as u8;
            let rule = match (method.as_str(), op) {
                ("Thermal_Information", OP_TEMPERATURE) => "temp ±2",
                ("Thermal_Information", OP_FAN_RPM) => "rpm ±100",
                ("Thermal_Information", OP_CURRENT_PROFILE) => "info",
                _ => "exact",
            };
            let ok = match (rule, n) {
                (_, None) => false,
                ("info", Some(_)) => true,
                ("temp ±2", Some(v)) => (v as i64 - *r as i64).abs() <= 2,
                ("rpm ±100", Some(v)) => (v as i64 - *r as i64).abs() <= 100,
                (_, Some(v)) => v == *r,
            };
            Compared { method: method.clone(), packed: *packed, reference: *r, now: n, rule, ok }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review finding 6: firmware errors and impossible values never become readings.
    #[test]
    fn decoders_reject_errors_and_implausible_values() {
        assert_eq!(decode_temp(98), Some(98));
        for bad in [0, 126, 255, ERROR] {
            assert_eq!(decode_temp(bad), None, "{bad}");
        }
        assert_eq!(decode_rpm(4600, Some(4200)), Some(4600), "above max is real (G-Mode)");
        assert_eq!(decode_rpm(6301, Some(4200)), None);
        assert_eq!(decode_rpm(0, Some(3000)), Some(0), "a stopped fan is a reading");
        assert_eq!(decode_rpm(ERROR, None), None);
        assert_eq!(decode_rpm(12_000, None), None);
        assert_eq!(decode_profile(0xAB), Some(0xAB));
        assert_eq!(decode_profile(ERROR), None);
    }

    /// P-05's comparison: identical passes; live values inside tolerance pass; a changed fan id or a missing
    /// answer fails; the profile is only reported.
    #[test]
    fn compare_is_exact_for_what_the_firmware_is_and_tolerant_for_live_values() {
        let reference = parse_capture(F1);
        assert!(compare(&reference, &reference).iter().all(|c| c.ok));
        let mut now = reference.clone();
        for row in now.iter_mut() {
            match ((row.1 & 0xFF) as u8, row.0.as_str()) {
                (OP_TEMPERATURE, "Thermal_Information") => row.2 += 2,
                (OP_FAN_RPM, "Thermal_Information") => row.2 += 100,
                (OP_CURRENT_PROFILE, "Thermal_Information") => row.2 = BALANCED as u32,
                _ => {}
            }
        }
        assert!(compare(&reference, &now).iter().all(|c| c.ok));
        now.iter_mut().find(|r| r.1 == read_temperature(0x01).packed()).unwrap().2 += 1;
        now.iter_mut().find(|r| r.1 == read_resource_id(2).packed()).unwrap().2 = 0x3D;
        now.retain(|r| r.1 != read_fan_max_rpm(0x3C).packed());
        let bad: Vec<_> = compare(&reference, &now).into_iter().filter(|c| !c.ok).map(|c| c.packed).collect();
        assert_eq!(bad, vec![read_resource_id(2).packed(), read_temperature(0x01).packed(), read_fan_max_rpm(0x3C).packed()]);
        let line = capture_line(&read_temperature(0x06), Some(63));
        assert_eq!(parse_capture(&line), vec![("Thermal_Information".to_string(), 0x0604, 63)]);
    }

    pub(crate) const F1: &str = include_str!("../tests/fixtures/f1-awcc-probe-2026-10-07.txt");

    pub(crate) fn replay() -> impl FnMut(Call) -> Option<u32> {
        let rows = parse_capture(F1);
        move |c: Call| rows.iter().find(|(m, p, _)| m == c.method.wmi_name() && *p == c.packed()).map(|r| r.2)
    }

    pub(crate) fn x17() -> Discovery {
        Discovery::run(replay()).unwrap()
    }

    #[test]
    /// 1 description + 12 resource ids + 1 profile + 2 temperatures + 4 fans × 4 ops + 4 fans × 2 GetFanSensors.
    fn the_capture_parses_into_40_rows() {
        assert_eq!(parse_capture(F1).len(), 1 + 12 + 1 + 2 + 16 + 8);
    }

    /// P-02: every packed value in the capture is what `pack` builds from its parts.
    #[test]
    fn packing_matches_every_captured_call() {
        for (_, packed, _) in parse_capture(F1) {
            let b = packed.to_le_bytes();
            assert_eq!(pack(b[0], b[1], b[2], b[3]), packed);
        }
        assert_eq!(read_temperature(0x06).packed(), 0x0000_0604);
        assert_eq!(read_fan_sensor_id(0x3C, 0).packed(), 0x0000_3C02);
    }

    /// P-01: exact decode of the x17 R2 capture.
    #[test]
    fn discovery_decodes_the_x17_exactly() {
        let d = x17();
        assert_eq!(d.description, SystemDescription { fans: 4, sensors: 2, unknown: 0, profiles: 6 });
        assert_eq!(d.fans, vec![0x32, 0x33, 0x3B, 0x3C]);
        assert_eq!(d.sensors, vec![0x01, 0x06]);
        assert_eq!(d.profiles, vec![0xA3, 0xA0, 0xA1, 0xA4, 0xA5, 0xAB]);
        assert_eq!(d.fan_sensor, BTreeMap::from([(0x32, 0x01), (0x33, 0x06), (0x3B, 0x01), (0x3C, 0x06)]));
        assert_eq!(d.partners(0x3B), vec![0x32]);
        assert_eq!(d.partners(0x33), vec![0x3C]);
        assert_eq!(d.lists()["profiles"], vec!["0xA3", "0xA0", "0xA1", "0xA4", "0xA5", "0xAB"]);
        let mut current = replay();
        assert_eq!(current(read_current_profile()), Some(GMODE as u32));
        assert_eq!(current(read_temperature(0x01)), Some(100));
        assert_eq!(current(read_fan_rpm(0x3B)), Some(0));
    }

    /// R-03: ids come from the firmware. A machine reporting 3 fans and 2 profiles gets exactly those.
    #[test]
    fn discovery_is_not_hard_coded() {
        let answers = std::collections::HashMap::from([
            (read_system_description(), pack(3, 1, 0, 2)),
            (read_resource_id(0), 0x40),
            (read_resource_id(1), 0x41),
            (read_resource_id(2), 0x42),
            (read_resource_id(3), 0x02),
            (read_resource_id(4), 0xA0),
            (read_resource_id(5), 0xA4),
        ]);
        let d = Discovery::run(|c| answers.get(&c).copied().or(if c.method == Method::GetFanSensors { Some(0) } else { None })).unwrap();
        assert_eq!(d.fans, vec![0x40, 0x41, 0x42]);
        assert_eq!(d.profiles, vec![0xA0, 0xA4]);
        assert!(d.fan_sensor.is_empty());
        assert_eq!(Discovery::run(|_| Some(ERROR)).unwrap_err(), Error::NoAnswer(read_system_description()));
    }

    /// R-01: the allow-list over every op of every method name, including the ones that must never be called.
    #[test]
    fn the_allow_list_is_exact() {
        let expect = |m: &str| -> Vec<u8> {
            match m {
                "Thermal_Information" => vec![0x02, 0x03, 0x04, 0x05, 0x08, 0x09, 0x0B, 0x0C],
                "GetFanSensors" => vec![0x01, 0x02],
                "Thermal_Control" => vec![0x01, 0x02],
                _ => vec![],
            }
        };
        let names = ["Thermal_Information", "GetFanSensors", "Thermal_Control", "SystemInformation", "PowerInformation", "BFn", "DoBFn", "Return_OverclockingReport", "Set_OCUIBIOSControl", "Clear_OCFailSafeFlag", "MemoryOCControl", "FWUpdateGPIOtoggle", "ReadGPIOpPinStatus", "CalderaSWInstallation", "TobiiCameraPowerOff", "thermal_control", ""];
        for name in names {
            let got: Vec<u8> = (0..=255u8).filter(|op| allowed(name, *op)).collect();
            assert_eq!(got, expect(name), "{name}");
        }
        // Every call the read builders can make is on the list.
        for c in [read_system_description(), read_resource_id(3), read_temperature(1), read_fan_rpm(0x32), read_fan_min_rpm(0x32), read_fan_max_rpm(0x32), read_current_profile(), read_fan_boost(0x32), read_fan_sensor_count(0x32), read_fan_sensor_id(0x32, 0)] {
            assert!(allowed(c.method.wmi_name(), c.op) && !c.is_write(), "{c:?}");
        }
    }

    /// C-01, C-02: writes only to ids the firmware reported; G-Mode only once cleared.
    #[test]
    fn writes_are_limited_to_reported_ids() {
        let d = x17();
        for &p in &[0xA3, 0xA0, 0xA1, 0xA4, 0xA5] {
            let c = d.write(Write::Profile(p), false).unwrap();
            assert_eq!(c.packed(), pack(OP_ACTIVATE_PROFILE, p, 0, 0));
            assert!(c.is_write() && allowed(c.method.wmi_name(), c.op));
        }
        assert_eq!(d.write(Write::Profile(0xA2), true), Err(Error::UnknownProfile(0xA2)), "valid in x17tune, not reported here");
        assert_eq!(d.write(Write::Profile(0x96), true), Err(Error::UnknownProfile(0x96)));
        assert_eq!(d.write(Write::Profile(GMODE), false), Err(Error::GModeNotCleared));
        assert!(d.write(Write::Profile(GMODE), true).is_ok());
        assert_eq!(d.write(Write::FanBoost { fan: 0x3D, level: 10 }, false), Err(Error::UnknownFan(0x3D)));
        assert_eq!(d.write(Write::FanBoost { fan: 0x33, level: 255 }, false).unwrap().packed(), 0x00FF_3302);
    }

    /// C-05: restore is balanced plus boost 0 on all four reported fans (x17tune resets only two).
    #[test]
    fn restore_resets_every_reported_fan() {
        let calls = x17().restore();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[0].packed(), pack(OP_ACTIVATE_PROFILE, BALANCED, 0, 0));
        let fans: Vec<u8> = calls[1..].iter().map(|c| c.arg1).collect();
        assert_eq!(fans, vec![0x32, 0x33, 0x3B, 0x3C]);
        assert!(calls[1..].iter().all(|c| c.op == OP_SET_FAN_BOOST && c.arg2 == 0));
    }
}
