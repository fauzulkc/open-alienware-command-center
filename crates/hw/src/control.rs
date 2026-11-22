//! Thermal-mode switching, the one write enabled so far (owner's ask, 2026-11-09). AWCC makes this same call
//! on this laptop every day; fan boost and curves stay locked until the cooling is checked and they pass
//! their tests.
//!
//! Every switch passes, in order: the machine gate (the model and BIOS the reads were verified on), a mode
//! the firmware itself reported (`Discovery::write`), no write when that mode is already set, the rate limit,
//! the write, then a read-back of the current mode until it matches (or the switch is reported unconfirmed).

use crate::Firmware;
use anyhow::{bail, Result};
use serde::Serialize;
use std::time::Duration;
use wiregent_oem_alienware::rules::RateLimit;
use wiregent_oem_alienware::{decode_profile, profile_name, read_current_profile, read_fan_boost, read_fan_rpm, Call, Discovery, Write};

/// A firmware that can also take writes.
pub trait FirmwareWrite: Firmware {
    fn write(&self, c: Call) -> Result<u32>;
}

impl<T: FirmwareWrite + ?Sized> FirmwareWrite for Box<T> {
    fn write(&self, c: Call) -> Result<u32> {
        (**self).write(c)
    }
}

#[cfg(windows)]
impl FirmwareWrite for crate::wmi::Awcc {
    fn write(&self, c: Call) -> Result<u32> {
        crate::wmi::Awcc::write(self, c)
    }
}

/// Machines and BIOS versions the firmware map was verified on.
pub const VERIFIED: &[(&str, &str)] = &[("Alienware x17 R2", "1.32.0")];

pub fn verified(model: &str, bios: &str) -> bool {
    VERIFIED.iter().any(|(m, b)| m.eq_ignore_ascii_case(model.trim()) && b.eq_ignore_ascii_case(bios.trim()))
}

/// Whether this machine is one the firmware map was verified on (model and SMBIOS BIOS version, from WMI).
#[cfg(windows)]
pub fn this_machine_verified() -> bool {
    let Ok(w) = crate::wmi::Wmi::connect(r"ROOT\CIMV2") else { return false };
    let model = w.query("SELECT Model FROM Win32_ComputerSystem", &["Model"]).ok().and_then(|r| r.first().map(|x| x[0].clone())).unwrap_or_default();
    let bios = w.query("SELECT SMBIOSBIOSVersion FROM Win32_BIOS", &["SMBIOSBIOSVersion"]).ok().and_then(|r| r.first().map(|x| x[0].clone())).unwrap_or_default();
    verified(&model, &bios)
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct ModeChange {
    pub requested: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The firmware reported the requested mode after the write.
    pub confirmed: bool,
    /// Nothing written: the mode was already set.
    pub unchanged: bool,
}

fn name(p: Option<u8>) -> Option<String> {
    p.map(|p| profile_name(p).to_string())
}

/// Parses "0xA4" (or "a4", "164") into a profile id.
pub fn parse_profile(s: &str) -> Option<u8> {
    let t = s.trim();
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
    match hex {
        Some(h) => u8::from_str_radix(h, 16).ok(),
        None => t.parse::<u8>().ok().or_else(|| u8::from_str_radix(t, 16).ok()),
    }
}

/// Switches the thermal mode. `machine_ok` is `verified(model, bios)` for this machine; `wait` sleeps between
/// read-backs (real time in use, a no-op in tests).
pub fn set_profile<F: FirmwareWrite + ?Sized>(fw: &F, d: &Discovery, profile: u8, machine_ok: bool, limit: &mut RateLimit, now_ms: u64, wait: &dyn Fn(Duration)) -> Result<ModeChange> {
    if !machine_ok {
        bail!("this laptop's model or BIOS isn't one the firmware map was verified on, so modes stay read-only");
    }
    // G-Mode is cleared on this unit: AWCC itself sets 0xAB here (seen in the 2026-11-09 key capture).
    let call = d.write(Write::Profile(profile), true).map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = fw.read(read_current_profile()).ok().and_then(decode_profile);
    if before == Some(profile) {
        return Ok(ModeChange { requested: profile_name(profile).into(), before: name(before), after: name(before), confirmed: true, unchanged: true });
    }
    if !limit.try_write(now_ms, false) {
        bail!("too many changes in a second; try again");
    }
    fw.write(call)?;
    let mut after = None;
    for _ in 0..12 {
        wait(Duration::from_millis(250));
        after = fw.read(read_current_profile()).ok().and_then(decode_profile);
        if after == Some(profile) {
            break;
        }
    }
    Ok(ModeChange { requested: profile_name(profile).into(), before: name(before), after: name(after), confirmed: after == Some(profile), unchanged: false })
}

/// The highest boost offered. alienfan treats values above 100 as "overboost" that needs per-fan calibration;
/// that stays off until it is measured on this laptop.
pub const MAX_BOOST: u8 = 100;

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct BoostChange {
    pub fan: String,
    pub level: u8,
    pub before: Option<u32>,
    /// The boost the firmware reports after the write (op 0x0C).
    pub after: Option<u32>,
    pub rpm_before: Option<u32>,
    pub rpm_after: Option<u32>,
    pub confirmed: bool,
}

/// Raises (or resets) one fan's boost. Up only in the sense that matters: boost adds to the firmware's own
/// curve, so no value here can make a fan slower than the firmware wants. Same gates as modes, plus 0..=100.
pub fn set_fan_boost<F: FirmwareWrite + ?Sized>(fw: &F, d: &Discovery, fan: u8, level: u8, machine_ok: bool, limit: &mut RateLimit, now_ms: u64, wait: &dyn Fn(Duration)) -> Result<BoostChange> {
    if !machine_ok {
        bail!("this laptop's model or BIOS isn't one the firmware map was verified on, so fan boost stays off");
    }
    if level > MAX_BOOST {
        bail!("boost above {MAX_BOOST} (overboost) needs a per-fan calibration on this laptop first");
    }
    let call = d.write(Write::FanBoost { fan, level }, false).map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = fw.read(read_fan_boost(fan)).ok();
    let rpm_before = fw.read(read_fan_rpm(fan)).ok();
    if before == Some(level as u32) {
        return Ok(BoostChange { fan: format!("0x{fan:02X}"), level, before, after: before, rpm_before, rpm_after: rpm_before, confirmed: true });
    }
    if !limit.try_write(now_ms, false) {
        bail!("too many changes in a second; try again");
    }
    fw.write(call)?;
    let mut after = None;
    for _ in 0..8 {
        wait(Duration::from_millis(250));
        after = fw.read(read_fan_boost(fan)).ok();
        if after == Some(level as u32) {
            break;
        }
    }
    // Give the fan a moment to respond, then read its speed again.
    wait(Duration::from_millis(1_500));
    let rpm_after = fw.read(read_fan_rpm(fan)).ok();
    Ok(BoostChange { fan: format!("0x{fan:02X}"), level, before, after, rpm_before, rpm_after, confirmed: after == Some(level as u32) })
}

/// Every reported fan back to boost 0 (the firmware's own curve). Used on exit, on stale readings, on stop.
/// Exempt from the rate limit; errors are collected, not raised, so one bad fan doesn't stop the rest.
pub fn reset_fans<F: FirmwareWrite + ?Sized>(fw: &F, d: &Discovery) -> Vec<String> {
    let mut errors = Vec::new();
    for &fan in &d.fans {
        if fw.read(read_fan_boost(fan)).ok() == Some(0) {
            continue;
        }
        match d.write(Write::FanBoost { fan, level: 0 }, false) {
            Ok(c) => {
                if let Err(e) = fw.write(c) {
                    errors.push(format!("0x{fan:02X}: {e:#}"));
                }
            }
            Err(e) => errors.push(format!("0x{fan:02X}: {e}")),
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::Replay;

    #[test]
    fn fan_boost_is_written_read_back_and_reset() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        let r = set_fan_boost(&fw, &d, 0x33, 60, true, &mut limit, 0, &|_| {}).unwrap();
        assert!(r.confirmed);
        assert_eq!((r.before, r.after), (Some(0), Some(60)));
        assert_eq!(fw.writes(), vec![0x003C_3302], "Thermal_Control op 0x02, fan 0x33, level 60");
        assert!(set_fan_boost(&fw, &d, 0x33, 101, true, &mut limit, 1, &|_| {}).is_err(), "no overboost");
        assert!(set_fan_boost(&fw, &d, 0x3D, 50, true, &mut limit, 1, &|_| {}).is_err(), "unreported fan");
        assert!(set_fan_boost(&fw, &d, 0x32, 50, false, &mut limit, 1, &|_| {}).is_err(), "unverified machine");
        assert!(reset_fans(&fw, &d).is_empty());
        assert_eq!(fw.writes().last(), Some(&0x0000_3302), "only the boosted fan is reset");
        assert_eq!(fw.read(read_fan_boost(0x33)).unwrap(), 0);
    }

    fn x17() -> (Replay, Discovery) {
        let fw = Replay::october();
        let d = Discovery::run(|c| fw.read(c).ok()).unwrap();
        (fw, d)
    }

    #[test]
    fn a_reported_mode_is_written_and_confirmed_by_read_back() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        let r = set_profile(&fw, &d, 0xA0, true, &mut limit, 0, &|_| {}).unwrap();
        assert_eq!((r.before.as_deref(), r.after.as_deref(), r.confirmed, r.unchanged), (Some("G-Mode"), Some("Balanced"), true, false));
        assert_eq!(fw.writes(), vec![0x0000_A001], "Thermal_Control op 0x01, profile 0xA0");
    }

    #[test]
    fn nothing_is_written_when_the_mode_is_already_set() {
        let (fw, d) = x17();
        let r = set_profile(&fw, &d, 0xAB, true, &mut RateLimit::default(), 0, &|_| {}).unwrap();
        assert!(r.unchanged && r.confirmed);
        assert!(fw.writes().is_empty());
    }

    #[test]
    fn gates_refuse_before_any_write() {
        let (fw, d) = x17();
        assert!(set_profile(&fw, &d, 0xA0, false, &mut RateLimit::default(), 0, &|_| {}).is_err(), "unverified BIOS");
        assert!(set_profile(&fw, &d, 0xA2, true, &mut RateLimit::default(), 0, &|_| {}).is_err(), "a mode this firmware didn't report");
        assert!(set_profile(&fw, &d, 0x00, true, &mut RateLimit::default(), 0, &|_| {}).is_err(), "Custom isn't reported here");
        assert!(fw.writes().is_empty());
        assert!(verified("Alienware x17 R2", "1.32.0") && !verified("Alienware x17 R2", "1.33.0") && !verified("Alienware m16", "1.32.0"));
    }

    #[test]
    fn a_switch_the_firmware_ignores_is_reported_unconfirmed() {
        let (fw, d) = x17();
        fw.ignore_writes();
        let r = set_profile(&fw, &d, 0xA3, true, &mut RateLimit::default(), 0, &|_| {}).unwrap();
        assert!(!r.confirmed);
        assert_eq!(r.after.as_deref(), Some("G-Mode"));
    }

    #[test]
    fn the_rate_limit_holds() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        for (i, p) in [0xA0u8, 0xA1, 0xA3, 0xA4, 0xA5].iter().enumerate() {
            set_profile(&fw, &d, *p, true, &mut limit, i as u64, &|_| {}).unwrap();
        }
        assert!(set_profile(&fw, &d, 0xA0, true, &mut limit, 6, &|_| {}).is_err(), "a sixth change within a second");
    }

    #[test]
    fn profile_ids_parse() {
        assert_eq!(parse_profile("0xA4"), Some(0xA4));
        assert_eq!(parse_profile("0Xab"), Some(0xAB));
        assert_eq!(parse_profile("a0"), Some(0xA0));
        assert_eq!(parse_profile("160"), Some(160));
        assert_eq!(parse_profile("0x1FF"), None);
    }
}
