//! The reader: discovery once, then one snapshot per tick with typed, checked values and the fault rules.

use crate::Firmware;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use wiregent_oem_alienware::rules::{FanFault, FanPairWatch, Sample, StuckReading, StuckWatch};
use wiregent_oem_alienware::*;

/// A sensor's role, by the ids the x17 R2 reported (0x01 follows the CPU fans, 0x06 the GPU fans).
pub fn sensor_name(id: u8) -> String {
    match id {
        0x01 => "CPU".into(),
        0x06 => "GPU".into(),
        _ => format!("sensor 0x{id:02X}"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SensorReading {
    pub id: u8,
    pub name: String,
    pub celsius: Option<i32>,
    /// No valid value for 30 s or more.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FanReading {
    pub id: u8,
    pub sensor: Option<u8>,
    pub rpm: Option<u32>,
    pub max_rpm: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Alert {
    /// A fan stood still while its sensor was hot and its partner spun.
    FanFault(FanFault),
    /// A fan's reported speed doesn't move while its partner's does.
    StuckReading(StuckReading),
    SensorStale { sensor: u8, name: String },
}

impl Alert {
    pub fn text(&self) -> String {
        match self {
            Alert::FanFault(f) => format!("Fan 0x{:02X} stood still for {} s while the {} sensor read up to {} °C and its partner spun: likely a fan fault. Check it before any fan control.", f.fan, (f.to_ms - f.from_ms) / 1000, sensor_name(f.sensor), f.peak_c),
            Alert::StuckReading(s) => format!("Fan 0x{:02X} has reported exactly {} RPM for {} s while its partner's speed moved {} RPM: its speed reading isn't a real measurement.", s.fan, s.rpm, (s.to_ms - s.from_ms) / 1000, s.partner_swing_rpm),
            Alert::SensorStale { name, .. } => format!("No valid {name} temperature for 30 s or more."),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub t_ms: u64,
    pub sensors: Vec<SensorReading>,
    pub fans: Vec<FanReading>,
    pub profile: Option<u8>,
    pub profile_name: Option<&'static str>,
    /// Alerts that fired on this tick (each fires once per episode).
    pub alerts: Vec<Alert>,
}

pub const STALE_MS: u64 = 30_000;

pub struct Reader<F: Firmware> {
    fw: F,
    pub discovery: Discovery,
    max_rpm: BTreeMap<u8, Option<u32>>,
    fan_pair: FanPairWatch,
    stuck: StuckWatch,
    last_valid: BTreeMap<u8, u64>,
    stale_reported: BTreeMap<u8, bool>,
    started_ms: Option<u64>,
}

impl<F: Firmware> Reader<F> {
    /// Discovery and the fans' static limits are read once.
    pub fn new(fw: F) -> Result<Self> {
        let discovery = Discovery::run(|c| fw.read(c).ok()).map_err(|e| anyhow!("{e}"))?;
        let max_rpm = discovery.fans.iter().map(|&f| (f, fw.read(read_fan_max_rpm(f)).ok().filter(|v| *v != 0 && *v != ERROR))).collect();
        Ok(Reader { fw, discovery, max_rpm, fan_pair: FanPairWatch::default(), stuck: StuckWatch::default(), last_valid: BTreeMap::new(), stale_reported: BTreeMap::new(), started_ms: None })
    }

    pub fn tick(&mut self, t_ms: u64) -> Snapshot {
        let started = *self.started_ms.get_or_insert(t_ms);
        let mut alerts = Vec::new();
        let mut temps = BTreeMap::new();
        let mut sensors = Vec::new();
        for &s in &self.discovery.sensors {
            let c = self.fw.read(read_temperature(s)).ok().and_then(decode_temp);
            if let Some(v) = c {
                temps.insert(s, v);
                self.last_valid.insert(s, t_ms);
                self.stale_reported.insert(s, false);
            }
            let stale = t_ms.saturating_sub(*self.last_valid.get(&s).unwrap_or(&started)) >= STALE_MS;
            if stale && !self.stale_reported.get(&s).copied().unwrap_or(false) {
                self.stale_reported.insert(s, true);
                alerts.push(Alert::SensorStale { sensor: s, name: sensor_name(s) });
            }
            sensors.push(SensorReading { id: s, name: sensor_name(s), celsius: c, stale });
        }
        let mut rpm = BTreeMap::new();
        let mut fans = Vec::new();
        for &f in &self.discovery.fans {
            let max = self.max_rpm.get(&f).copied().flatten();
            let r = self.fw.read(read_fan_rpm(f)).ok().and_then(|v| decode_rpm(v, max));
            if let Some(v) = r {
                rpm.insert(f, v);
            }
            fans.push(FanReading { id: f, sensor: self.discovery.fan_sensor.get(&f).copied(), rpm: r, max_rpm: max });
        }
        let profile = self.fw.read(read_current_profile()).ok().and_then(decode_profile);
        let sample = Sample { t_ms, temps, rpm };
        alerts.extend(self.fan_pair.push(&self.discovery, &sample).into_iter().map(Alert::FanFault));
        alerts.extend(self.stuck.push(&self.discovery, &sample).into_iter().map(Alert::StuckReading));
        Snapshot { t_ms, sensors, fans, profile, profile_name: profile.map(profile_name), alerts }
    }

    /// After sleep: staleness clocks and the fault rules start over, so waking never looks like a stale
    /// sensor or a fan that stopped for the whole sleep (design §6).
    pub fn resumed(&mut self) {
        self.last_valid.clear();
        self.stale_reported.clear();
        self.started_ms = None;
        self.fan_pair = FanPairWatch::default();
        self.stuck = StuckWatch::default();
    }

    /// The firmware this reader owns (the controller writes through it: one connection, one writer).
    pub fn firmware(&self) -> &F {
        &self.fw
    }

    /// The enum lists the plugin host checks action arguments against.
    pub fn lists(&self) -> BTreeMap<String, Vec<String>> {
        self.discovery.lists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::Replay;

    #[test]
    fn a_snapshot_from_the_october_capture() {
        let mut r = Reader::new(Replay::october()).unwrap();
        let s = r.tick(0);
        assert_eq!(s.profile, Some(0xAB));
        assert_eq!(s.profile_name, Some("G-Mode"));
        assert_eq!(s.sensors.iter().map(|x| (x.name.as_str(), x.celsius)).collect::<Vec<_>>(), vec![("CPU", Some(100)), ("GPU", Some(63))]);
        let fan = |id| s.fans.iter().find(|f| f.id == id).unwrap().clone();
        assert_eq!(fan(0x32).rpm, Some(4600));
        assert_eq!(fan(0x32).max_rpm, Some(4200));
        assert_eq!(fan(0x3B).rpm, Some(0));
        assert_eq!(fan(0x3B).sensor, Some(0x01));
        assert!(s.alerts.is_empty());
        assert_eq!(r.lists()["fans"], vec!["0x32", "0x33", "0x3B", "0x3C"]);
    }

    /// A firmware that stops answering temperatures: the sensor goes stale after 30 s, alerted once.
    #[test]
    fn a_sensor_that_stops_answering_goes_stale_once() {
        let fw = Replay::october();
        let mut r = Reader::new(fw.clone()).unwrap();
        r.tick(0);
        fw.fail_op(OP_TEMPERATURE);
        let s = r.tick(10_000);
        assert!(s.sensors.iter().all(|x| x.celsius.is_none() && !x.stale));
        let s = r.tick(31_000);
        assert!(s.sensors.iter().all(|x| x.stale));
        assert_eq!(s.alerts.iter().filter(|a| matches!(a, Alert::SensorStale { .. })).count(), 2);
        let s = r.tick(40_000);
        assert!(s.alerts.is_empty(), "reported once per episode");
    }

    /// Two minutes of the October trace raise the fan-fault alert for 0x3B.
    #[test]
    fn the_fan_fault_alert_fires_from_live_ticks() {
        let mut r = Reader::new(Replay::october()).unwrap();
        let mut fired = Vec::new();
        for i in 0..=62 {
            fired.extend(r.tick(i * 2_000).alerts);
        }
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert!(matches!(&fired[0], Alert::FanFault(f) if f.fan == 0x3B));
        // The replay repeats one snapshot, so the partner never moves: no stuck-reading alert from it.
    }
}
