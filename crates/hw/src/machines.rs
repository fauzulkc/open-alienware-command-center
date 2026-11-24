//! The [`Machine`] implementations, and [`select`], which picks one by name.
//!
//! - [`ReplayMachine`]: the recorded x17 R2 capture (demo data and tests). Writes go to the replay only.
//! - [`AwccMachine`] (Windows): Dell's AWCC WMI interface, directly; needs an elevated process.
//! - [`ServiceMachine`] (Windows): through the hardware service's pipe, which holds the firmware connection
//!   and runs its own emergency rule.
//! - Linux: `crate::linux::LinuxSysfs` (the kernel's alienware-wmi driver), written in its own module.
//!
//! None of these has gate logic of its own: the replay and AWCC machines call `control::set_profile`,
//! `set_fan_boost`, `reset_fans` and `apply_emergency`, and the service checks every request again with the
//! same functions on its side.

use crate::control::{self, BoostChange, ModeChange};
use crate::machine::Machine;
use crate::reader::{Alert, FanReading, Reader, SensorReading, Snapshot};
use crate::replay::Replay;
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use wiregent_oem_alienware::profile_name;
use wiregent_oem_alienware::rules::{Emergency, FanFault, RateLimit, StuckReading, ZoneStopped};

/// The backend names [`select`] takes.
pub const BACKENDS: &[&str] = &["auto", "service", "direct", "linux", "demo"];

/// The recorded capture as a machine. It is the verified unit's own capture (x17 R2, BIOS 1.32.0), so the
/// gates treat it as verified; writes change only the replay's state.
pub struct ReplayMachine {
    reader: Reader<Replay>,
    limit: RateLimit,
}

impl ReplayMachine {
    pub fn new(fw: Replay) -> Result<Self> {
        Ok(ReplayMachine { reader: Reader::new(fw)?, limit: RateLimit::default() })
    }

    /// The x17 R2 capture of 2026-10-07 (G-Mode, CPU 100 °C, fan 0x3B at 0 RPM).
    pub fn october() -> Result<Self> {
        Self::new(Replay::october())
    }

    /// The replay underneath (tests: what was written, a fan that stops).
    pub fn firmware(&self) -> &Replay {
        self.reader.firmware()
    }
}

impl Machine for ReplayMachine {
    fn backend(&self) -> &'static str {
        "replay"
    }

    fn identity(&self) -> (String, String) {
        let (model, bios) = control::VERIFIED[0];
        (model.into(), bios.into())
    }

    fn snapshot(&mut self, t_ms: u64) -> Snapshot {
        self.reader.tick(t_ms)
    }

    fn profiles(&self) -> Vec<u8> {
        self.reader.discovery.profiles.clone()
    }

    fn fans(&self) -> Vec<u8> {
        self.reader.discovery.fans.clone()
    }

    fn set_profile(&mut self, profile: u8, now_ms: u64) -> Result<ModeChange> {
        control::set_profile(self.reader.firmware(), &self.reader.discovery, profile, true, &mut self.limit, now_ms, &|_| {})
    }

    fn set_fan_boost(&mut self, fan: u8, level: u8, now_ms: u64) -> Result<BoostChange> {
        control::set_fan_boost(self.reader.firmware(), &self.reader.discovery, fan, level, true, &mut self.limit, now_ms, &|_| {})
    }

    fn reset_fans(&mut self) -> Vec<String> {
        control::reset_fans(self.reader.firmware(), &self.reader.discovery)
    }

    fn apply_emergency(&mut self, e: Emergency, person: &BTreeMap<u8, u8>) -> Vec<String> {
        control::apply_emergency(self.reader.firmware(), &self.reader.discovery, e, person, true)
    }
}

/// Model and BIOS version from Windows (Win32_ComputerSystem, Win32_BIOS); empty when WMI doesn't say.
#[cfg(windows)]
pub fn system_identity() -> (String, String) {
    let Ok(w) = crate::wmi::Wmi::connect(r"ROOT\CIMV2") else { return Default::default() };
    let first = |q: &str, p: &str| w.query(q, &[p]).ok().and_then(|r| r.first().and_then(|x| x.first().cloned())).unwrap_or_default();
    (first("SELECT Model FROM Win32_ComputerSystem", "Model"), first("SELECT SMBIOSBIOSVersion FROM Win32_BIOS", "SMBIOSBIOSVersion"))
}

/// Dell's AWCC WMI interface, read and written by this process (it must be elevated). The machine gate is
/// asked again on every write (`control::this_machine_verified`), as the app does.
#[cfg(windows)]
pub struct AwccMachine {
    reader: Reader<crate::wmi::Awcc>,
    limit: RateLimit,
    identity: (String, String),
}

#[cfg(windows)]
impl AwccMachine {
    pub fn open() -> Result<Self> {
        let fw = crate::wmi::Awcc::open()?;
        Ok(AwccMachine { reader: Reader::new(fw)?, limit: RateLimit::default(), identity: system_identity() })
    }
}

#[cfg(windows)]
impl Machine for AwccMachine {
    fn backend(&self) -> &'static str {
        "awcc-wmi"
    }

    fn identity(&self) -> (String, String) {
        self.identity.clone()
    }

    fn snapshot(&mut self, t_ms: u64) -> Snapshot {
        self.reader.tick(t_ms)
    }

    fn profiles(&self) -> Vec<u8> {
        self.reader.discovery.profiles.clone()
    }

    fn fans(&self) -> Vec<u8> {
        self.reader.discovery.fans.clone()
    }

    fn set_profile(&mut self, profile: u8, now_ms: u64) -> Result<ModeChange> {
        control::set_profile(self.reader.firmware(), &self.reader.discovery, profile, control::this_machine_verified(), &mut self.limit, now_ms, &std::thread::sleep)
    }

    fn set_fan_boost(&mut self, fan: u8, level: u8, now_ms: u64) -> Result<BoostChange> {
        control::set_fan_boost(self.reader.firmware(), &self.reader.discovery, fan, level, control::this_machine_verified(), &mut self.limit, now_ms, &std::thread::sleep)
    }

    fn reset_fans(&mut self) -> Vec<String> {
        control::reset_fans(self.reader.firmware(), &self.reader.discovery)
    }

    fn apply_emergency(&mut self, e: Emergency, person: &BTreeMap<u8, u8>) -> Vec<String> {
        control::apply_emergency(self.reader.firmware(), &self.reader.discovery, e, person, control::this_machine_verified())
    }
}

/// What a client gets when it asks for the emergency rule through the service: the service runs its own.
pub const SERVICE_RUNS_EMERGENCY: &str = "the hardware service runs its own emergency rule; a client can't drive it";

/// The hardware service, through its pipe. Every write is checked again in the service (machine gate,
/// reported ids, its own rate limit), so this side only forwards.
#[cfg(windows)]
pub struct ServiceMachine {
    profiles: Vec<u8>,
    fans: Vec<u8>,
    identity: (String, String),
}

#[cfg(windows)]
impl ServiceMachine {
    pub fn open() -> Result<Self> {
        if !crate::client::service_present() {
            bail!("the hardware service isn't running");
        }
        let lists = match ask(&crate::proto::Request::Lists)? {
            crate::proto::Response::Lists { lists } => lists,
            other => bail!("unexpected answer from the hardware service: {other:?}"),
        };
        let ids = |k: &str| lists[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().and_then(control::parse_profile)).collect::<Vec<u8>>();
        Ok(ServiceMachine { profiles: ids("profiles"), fans: ids("fans"), identity: system_identity() })
    }
}

/// One request to the real service (its process checked, not just its name); an `Error` answer is an error.
#[cfg(windows)]
fn ask(req: &crate::proto::Request) -> Result<crate::proto::Response> {
    use crate::proto::{Response, PIPE_NAME};
    match crate::client::request(PIPE_NAME, req, crate::client::SERVICE_IMAGE)? {
        Response::Error { message } => bail!("{message}"),
        r => Ok(r),
    }
}

#[cfg(windows)]
impl Machine for ServiceMachine {
    fn backend(&self) -> &'static str {
        "service"
    }

    fn identity(&self) -> (String, String) {
        self.identity.clone()
    }

    /// The service's latest reading. A failed request gives an empty snapshot (no sensors, no fans): the
    /// trait has no error here, and an empty reading is never mistaken for a cool one.
    fn snapshot(&mut self, t_ms: u64) -> Snapshot {
        match ask(&crate::proto::Request::Snapshot) {
            Ok(crate::proto::Response::Snapshot { snapshot: Some(v), .. }) => snapshot_from_json(&v, t_ms),
            _ => Snapshot { t_ms, sensors: vec![], fans: vec![], profile: None, profile_name: None, alerts: vec![] },
        }
    }

    fn profiles(&self) -> Vec<u8> {
        self.profiles.clone()
    }

    fn fans(&self) -> Vec<u8> {
        self.fans.clone()
    }

    fn set_profile(&mut self, profile: u8, _now_ms: u64) -> Result<ModeChange> {
        match ask(&crate::proto::Request::SetProfile { profile: format!("0x{profile:02X}") })? {
            crate::proto::Response::ModeChanged { change } => Ok(serde_json::from_value(change)?),
            other => bail!("unexpected answer from the hardware service: {other:?}"),
        }
    }

    fn set_fan_boost(&mut self, fan: u8, level: u8, _now_ms: u64) -> Result<BoostChange> {
        match ask(&crate::proto::Request::SetFanBoost { fan: format!("0x{fan:02X}"), level })? {
            crate::proto::Response::BoostChanged { change } => Ok(serde_json::from_value(change)?),
            other => bail!("unexpected answer from the hardware service: {other:?}"),
        }
    }

    /// Each reported fan back to 0 through SetFanBoost (the service has no reset request). A fan already at 0
    /// is answered without a write on the service's side.
    fn reset_fans(&mut self) -> Vec<String> {
        self.fans.clone().into_iter().filter_map(|f| self.set_fan_boost(f, 0, 0).err().map(|e| format!("0x{f:02X}: {e:#}"))).collect()
    }

    fn apply_emergency(&mut self, _e: Emergency, _person: &BTreeMap<u8, u8>) -> Vec<String> {
        vec![SERVICE_RUNS_EMERGENCY.into()]
    }
}

/// A reader snapshot back from its JSON (the service sends snapshots as JSON). Unknown alert kinds are
/// dropped; `t_ms` is the caller's clock.
pub fn snapshot_from_json(v: &Value, t_ms: u64) -> Snapshot {
    let byte = |x: &Value| x.as_u64().and_then(|n| u8::try_from(n).ok());
    let num = |x: &Value| x.as_u64().and_then(|n| u32::try_from(n).ok());
    let sensors = v["sensors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some(SensorReading { id: byte(&s["id"])?, name: s["name"].as_str().unwrap_or_default().to_string(), celsius: s["celsius"].as_i64().map(|c| c as i32), stale: s["stale"].as_bool().unwrap_or(false) }))
        .collect();
    let fans = v["fans"].as_array().into_iter().flatten().filter_map(|f| Some(FanReading { id: byte(&f["id"])?, sensor: byte(&f["sensor"]), rpm: num(&f["rpm"]), max_rpm: num(&f["max_rpm"]) })).collect();
    let alerts = v["alerts"].as_array().into_iter().flatten().filter_map(alert_from_json).collect();
    let profile = byte(&v["profile"]);
    Snapshot { t_ms, sensors, fans, profile, profile_name: profile.map(profile_name), alerts }
}

fn alert_from_json(a: &Value) -> Option<Alert> {
    let byte = |k: &str| a[k].as_u64().and_then(|n| u8::try_from(n).ok());
    let ms = |k: &str| a[k].as_u64();
    let num = |k: &str| a[k].as_u64().and_then(|n| u32::try_from(n).ok());
    let celsius = |k: &str| a[k].as_i64().and_then(|n| i32::try_from(n).ok());
    Some(match a["kind"].as_str()? {
        "fan-fault" => Alert::FanFault(FanFault { fan: byte("fan")?, sensor: byte("sensor")?, from_ms: ms("from_ms")?, to_ms: ms("to_ms")?, peak_c: celsius("peak_c")? }),
        "stuck-reading" => Alert::StuckReading(StuckReading { fan: byte("fan")?, rpm: num("rpm")?, from_ms: ms("from_ms")?, to_ms: ms("to_ms")?, partner_swing_rpm: num("partner_swing_rpm")? }),
        "zone-stopped" => Alert::ZoneStopped(ZoneStopped { sensor: byte("sensor")?, fans: a["fans"].as_array()?.iter().filter_map(|f| f.as_u64().and_then(|n| u8::try_from(n).ok())).collect(), from_ms: ms("from_ms")?, to_ms: ms("to_ms")?, peak_c: celsius("peak_c")? }),
        "sensor-stale" => Alert::SensorStale { sensor: byte("sensor")?, name: a["name"].as_str()?.to_string() },
        _ => return None,
    })
}

/// A machine by backend name: "auto", "service", "direct", "linux" or "demo".
///
/// - Windows, auto: the hardware service if it runs, else the firmware directly if this process is
///   elevated, else an error that says how to get readings.
/// - Linux, auto: the alienware-wmi driver through sysfs.
/// - demo: the recorded capture, on any system.
pub fn select(backend: &str) -> Result<Box<dyn Machine>> {
    match backend {
        "demo" => Ok(Box::new(ReplayMachine::october()?)),
        "auto" => auto(),
        "service" => service(),
        "direct" => direct(),
        "linux" => linux(),
        other => bail!("unknown backend '{other}': use one of {}", BACKENDS.join(", ")),
    }
}

#[cfg(windows)]
fn auto() -> Result<Box<dyn Machine>> {
    if crate::client::service_present() {
        service()
    } else if crate::is_admin() {
        direct()
    } else {
        bail!("no live readings: install the hardware service, or run this from an administrator terminal (--backend demo shows the recorded capture)")
    }
}

#[cfg(target_os = "linux")]
fn auto() -> Result<Box<dyn Machine>> {
    linux()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn auto() -> Result<Box<dyn Machine>> {
    bail!("no backend for this system: Windows (AWCC) and Linux (alienware-wmi) only; --backend demo shows the recorded capture")
}

#[cfg(windows)]
fn service() -> Result<Box<dyn Machine>> {
    Ok(Box::new(ServiceMachine::open()?))
}

#[cfg(not(windows))]
fn service() -> Result<Box<dyn Machine>> {
    bail!("the hardware service is Windows only")
}

#[cfg(windows)]
fn direct() -> Result<Box<dyn Machine>> {
    if !crate::is_admin() {
        bail!("direct firmware access needs an administrator terminal")
    }
    Ok(Box::new(AwccMachine::open()?))
}

#[cfg(not(windows))]
fn direct() -> Result<Box<dyn Machine>> {
    bail!("direct AWCC access is Windows only; on Linux use --backend linux")
}

#[cfg(target_os = "linux")]
fn linux() -> Result<Box<dyn Machine>> {
    Ok(Box::new(crate::linux::LinuxSysfs::open_default()?))
}

#[cfg(not(target_os = "linux"))]
fn linux() -> Result<Box<dyn Machine>> {
    bail!("the linux backend (alienware-wmi through sysfs) runs on Linux only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiregent_oem_alienware::read_fan_boost;

    fn boost(m: &ReplayMachine, fan: u8) -> u32 {
        crate::Firmware::read(m.firmware(), read_fan_boost(fan)).unwrap()
    }

    #[test]
    fn the_replay_machine_reads_like_the_verified_unit() {
        let mut m = ReplayMachine::october().unwrap();
        assert_eq!(m.backend(), "replay");
        let (model, bios) = m.identity();
        assert!(control::verified(&model, &bios), "{model} {bios}");
        assert_eq!(m.profiles(), vec![0xA3, 0xA0, 0xA1, 0xA4, 0xA5, 0xAB]);
        assert_eq!(m.fans(), vec![0x32, 0x33, 0x3B, 0x3C]);
        let s = m.snapshot(0);
        assert_eq!(s.profile_name, Some("G-Mode"));
        assert_eq!(s.sensors.iter().map(|x| x.celsius).collect::<Vec<_>>(), vec![Some(100), Some(63)]);
        assert_eq!(s.fans.len(), 4);
    }

    #[test]
    fn the_replay_machine_switches_modes_and_fans_through_the_gates() {
        let mut m = ReplayMachine::october().unwrap();
        let c = m.set_profile(0xA0, 0).unwrap();
        assert!(c.confirmed && !c.unchanged);
        assert_eq!(m.snapshot(1).profile_name, Some("Balanced"));
        let b = m.set_fan_boost(0x33, 60, 1).unwrap();
        assert!(b.confirmed);
        assert_eq!(boost(&m, 0x33), 60);
        assert!(m.reset_fans().is_empty());
        assert_eq!(boost(&m, 0x33), 0);
    }

    #[test]
    fn the_replay_machine_runs_the_emergency_rule() {
        let mut m = ReplayMachine::october().unwrap();
        assert!(m.apply_emergency(Emergency::Engage, &BTreeMap::new()).is_empty());
        assert!(m.fans().iter().all(|f| boost(&m, *f) == control::EMERGENCY_BOOST as u32));
        assert!(m.apply_emergency(Emergency::Release, &BTreeMap::from([(0x32u8, 30u8)])).is_empty());
        assert_eq!(m.fans().iter().map(|f| boost(&m, *f)).collect::<Vec<_>>(), vec![30, 0, 0, 0]);
    }

    /// Refusals (overboost, a fan or mode the firmware didn't report, the rate limit) never reach the firmware.
    #[test]
    fn refusals_write_nothing() {
        let mut m = ReplayMachine::october().unwrap();
        assert!(m.set_fan_boost(0x32, 101, 0).is_err());
        assert!(m.set_fan_boost(0x3D, 50, 0).is_err());
        assert!(m.set_profile(0xA2, 0).is_err());
        assert!(m.firmware().writes().is_empty());
        for (i, level) in [10u8, 20, 30, 40, 50].iter().enumerate() {
            m.set_fan_boost(0x33, *level, i as u64).unwrap();
        }
        assert!(m.set_fan_boost(0x33, 60, 6).is_err(), "the sixth within a second");
        assert_eq!(m.firmware().writes().len(), 5);
    }

    /// The service sends snapshots as JSON; they come back the same.
    #[test]
    fn snapshots_survive_json() {
        let fw = Replay::october();
        fw.set_rpm(0x32, 0);
        let mut m = ReplayMachine::new(fw).unwrap();
        let mut alerts = Vec::new();
        let mut last = None;
        for i in 0..=62 {
            let s = m.snapshot(i * 1_000);
            alerts.extend(s.alerts.clone());
            last = Some(s);
        }
        let mut s = last.unwrap();
        s.alerts = alerts;
        s.alerts.push(Alert::SensorStale { sensor: 6, name: "GPU".into() });
        assert!(s.alerts.len() >= 2, "{:?}", s.alerts);
        let v = serde_json::to_value(&s).unwrap();
        let back = snapshot_from_json(&v, s.t_ms);
        assert_eq!(serde_json::to_value(&back).unwrap(), v);
        let odd = serde_json::json!({ "alerts": [{ "kind": "sensor-stale", "sensor": 1, "name": "CPU" }, { "kind": "from-the-future" }] });
        assert_eq!(snapshot_from_json(&odd, 0).alerts.len(), 1, "unknown kinds dropped");
        assert!(snapshot_from_json(&Value::Null, 0).sensors.is_empty());
    }

    #[test]
    fn select_knows_its_backends() {
        assert_eq!(select("demo").unwrap().backend(), "replay");
        assert!(select("nonsense").is_err());
        #[cfg(not(target_os = "linux"))]
        assert!(select("linux").is_err());
        #[cfg(not(windows))]
        assert!(select("service").is_err() && select("direct").is_err());
    }
}
