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

/// The boost the emergency rule sets on every fan: the highest this laptop has been checked at. The rule's
/// first draft said 255, but values above 100 are uncalibrated "overboost" (see [`MAX_BOOST`]).
pub const EMERGENCY_BOOST: u8 = MAX_BOOST;

/// Carries out an emergency rule decision. Engage: every reported fan to [`EMERGENCY_BOOST`], whatever the
/// person picked. Release: each fan back to the person's own boost (0, the firmware's curve, for fans they
/// never raised). Never rate-limited (a person's five quick clicks must not hold back cooling), but still
/// gated on the verified machine: on firmware nobody checked, the rule only alerts. Errors are collected,
/// not raised, so one bad fan doesn't stop the others.
pub fn apply_emergency<F: FirmwareWrite + ?Sized>(fw: &F, d: &Discovery, e: wiregent_oem_alienware::rules::Emergency, person: &std::collections::BTreeMap<u8, u8>, machine_ok: bool) -> Vec<String> {
    use wiregent_oem_alienware::rules::Emergency;
    if !machine_ok {
        return vec!["this laptop's model or BIOS isn't verified, so the emergency rule can only warn".into()];
    }
    let mut errors = Vec::new();
    for &fan in &d.fans {
        let level = match e {
            Emergency::Engage => EMERGENCY_BOOST,
            Emergency::Release => person.get(&fan).copied().unwrap_or(0).min(MAX_BOOST),
        };
        if fw.read(read_fan_boost(fan)).ok() == Some(level as u32) {
            continue;
        }
        match d.write(Write::FanBoost { fan, level }, false) {
            Ok(c) => {
                if let Err(err) = fw.write(c) {
                    errors.push(format!("0x{fan:02X}: {err:#}"));
                }
            }
            Err(err) => errors.push(format!("0x{fan:02X}: {err}")),
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::Replay;
    use wiregent_oem_alienware::rules::Emergency;

    /// C-07/C-08: engaging sets every fan to full, even right after the person used up the rate limit.
    #[test]
    fn emergency_engage_boosts_every_fan_past_the_rate_limit() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        for i in 0..5 {
            set_fan_boost(&fw, &d, 0x33, 10 + i as u8, true, &mut limit, i, &|_| {}).unwrap();
        }
        assert!(!limit.try_write(6, false), "the person's budget for this second is spent");
        assert!(apply_emergency(&fw, &d, Emergency::Engage, &Default::default(), true).is_empty());
        for fan in [0x32u8, 0x33, 0x3B, 0x3C] {
            assert_eq!(fw.read(read_fan_boost(fan)).unwrap(), EMERGENCY_BOOST as u32, "fan 0x{fan:02X}");
        }
    }

    /// C-09: release hands each fan back to what the person picked, and the rest to the firmware's curve.
    #[test]
    fn emergency_release_restores_the_persons_boost() {
        let (fw, d) = x17();
        apply_emergency(&fw, &d, Emergency::Engage, &Default::default(), true);
        let person = std::collections::BTreeMap::from([(0x33u8, 60u8)]);
        assert!(apply_emergency(&fw, &d, Emergency::Release, &person, true).is_empty());
        let got: Vec<u32> = [0x32u8, 0x33, 0x3B, 0x3C].iter().map(|f| fw.read(read_fan_boost(*f)).unwrap()).collect();
        assert_eq!(got, vec![0, 60, 0, 0]);
    }

    /// On an unverified machine the rule warns and writes nothing.
    #[test]
    fn emergency_on_an_unverified_machine_writes_nothing() {
        let (fw, d) = x17();
        assert_eq!(apply_emergency(&fw, &d, Emergency::Engage, &Default::default(), false).len(), 1);
        assert!(fw.writes().is_empty());
    }

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
        assert_eq!(fw.writes().len(), 5, "the refused sixth never reached the firmware");
    }

    /// Every refusal happens before the firmware sees anything: the count of writes doesn't move.
    #[test]
    fn fan_boost_gates_refuse_before_any_write() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        assert!(set_fan_boost(&fw, &d, 0x32, 50, false, &mut limit, 0, &|_| {}).is_err(), "unverified machine");
        assert!(set_fan_boost(&fw, &d, 0x32, MAX_BOOST + 1, true, &mut limit, 0, &|_| {}).is_err(), "overboost");
        assert!(set_fan_boost(&fw, &d, 0x3D, 50, true, &mut limit, 0, &|_| {}).is_err(), "a fan this firmware didn't report");
        assert!(fw.writes().is_empty(), "{:X?}", fw.writes());
    }

    /// C-11 for fans: the sixth boost within a second is refused and never reaches the firmware.
    #[test]
    fn fan_boost_rate_limit_refuses_the_sixth_and_writes_nothing() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        for (i, level) in [10u8, 20, 30, 40, 50].iter().enumerate() {
            set_fan_boost(&fw, &d, 0x33, *level, true, &mut limit, i as u64, &|_| {}).unwrap();
        }
        assert!(set_fan_boost(&fw, &d, 0x33, 60, true, &mut limit, 6, &|_| {}).is_err());
        assert_eq!(fw.writes().len(), 5);
        assert!(set_fan_boost(&fw, &d, 0x33, 60, true, &mut limit, 1_010, &|_| {}).is_ok(), "a second later");
    }

    #[test]
    fn a_boost_the_firmware_ignores_is_reported_unconfirmed() {
        let (fw, d) = x17();
        fw.ignore_writes();
        let r = set_fan_boost(&fw, &d, 0x33, 60, true, &mut RateLimit::default(), 0, &|_| {}).unwrap();
        assert!(!r.confirmed);
        assert_eq!(r.after, Some(0));
        assert_eq!(fw.writes().len(), 1);
    }

    /// An unreadable model or BIOS (WMI failed: "") or a near miss is not the verified machine.
    #[test]
    fn the_machine_gate_refuses_empty_and_partial() {
        for (m, b) in [("", ""), ("Alienware x17 R2", ""), ("", "1.32.0"), ("Alienware x17 R2", "1.32"), ("Alienware x17 R2", "1.32.0.1"), ("Alienware x17", "1.32.0"), ("Alienware x17 R2 Special", "1.32.0")] {
            assert!(!verified(m, b), "{m:?} {b:?}");
        }
        assert!(verified(" alienware X17 r2 ", " 1.32.0 "), "spacing and case from WMI don't matter");
    }

    /// The reset puts every boosted fan back, not just the first one it finds.
    #[test]
    fn reset_fans_resets_every_boosted_fan() {
        let (fw, d) = x17();
        let mut limit = RateLimit::default();
        for (i, fan) in [0x32u8, 0x33, 0x3C].iter().enumerate() {
            set_fan_boost(&fw, &d, *fan, 70, true, &mut limit, i as u64, &|_| {}).unwrap();
        }
        assert!(reset_fans(&fw, &d).is_empty());
        for fan in [0x32u8, 0x33, 0x3B, 0x3C] {
            assert_eq!(fw.read(read_fan_boost(fan)).unwrap(), 0, "fan 0x{fan:02X}");
        }
        assert_eq!(fw.writes().len(), 6, "three raises, three resets; the fan never raised isn't written");
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
