//! Health: battery wear, disk wear, and crash / hardware-error events (parity rows 29 and 35). Read-only.
//! Each part says when it couldn't be read and why, instead of guessing.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Battery {
    pub design_mwh: u32,
    pub full_mwh: u32,
    /// How much capacity is gone, in percent of the design capacity.
    pub wear_pct: f64,
    pub cycles: Option<u32>,
}

/// Battery wear from the design and full-charge capacities; None when either is missing or nonsense.
pub fn battery(design_mwh: u32, full_mwh: u32, cycles: Option<u32>) -> Option<Battery> {
    if design_mwh == 0 || full_mwh == 0 || full_mwh > design_mwh.saturating_mul(2) {
        return None;
    }
    let wear = (1.0 - full_mwh as f64 / design_mwh as f64) * 100.0;
    Some(Battery { design_mwh, full_mwh, wear_pct: (wear.max(0.0) * 10.0).round() / 10.0, cycles })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Disk {
    pub name: String,
    /// Percent of rated endurance used (NVMe "percentage used"), when the drive reports it.
    pub wear_pct: Option<u32>,
    pub temp_c: Option<u32>,
    pub read_errors: Option<u64>,
    pub power_on_hours: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Events {
    pub days: u32,
    /// Hardware errors reported by the CPU, memory or PCIe (WHEA-Logger).
    pub hardware_errors: u64,
    /// Blue screens (BugCheck 1001).
    pub crashes: u64,
    /// Power lost or the machine stopped without shutting down (Kernel-Power 41).
    pub unexpected_shutdowns: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Health {
    pub battery: Option<Battery>,
    pub disks: Vec<Disk>,
    pub events: Option<Events>,
    /// Parts that couldn't be read, with the reason ("disk wear: needs admin").
    pub unavailable: Vec<String>,
    /// Plain-language findings, worst first.
    pub findings: Vec<String>,
}

/// The findings a person should see, from the numbers.
pub fn findings(h: &Health) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(e) = &h.events {
        if e.crashes > 0 {
            out.push(format!("{} blue screen(s) in the last {} days.", e.crashes, e.days));
        }
        if e.hardware_errors > 0 {
            out.push(format!("{} hardware error report(s) (WHEA) in the last {} days: CPU, memory or PCIe errors the hardware corrected or reported.", e.hardware_errors, e.days));
        }
        if e.unexpected_shutdowns > 0 {
            out.push(format!("{} unexpected shutdown(s) in the last {} days (power lost, or the machine stopped responding).", e.unexpected_shutdowns, e.days));
        }
    }
    for d in &h.disks {
        if let Some(w) = d.wear_pct.filter(|w| *w >= 80) {
            out.push(format!("{} has used {w}% of its rated write endurance.", d.name));
        }
        if d.read_errors.is_some_and(|n| n > 0) {
            out.push(format!("{} reports {} read error(s).", d.name, d.read_errors.unwrap_or(0)));
        }
    }
    if let Some(b) = h.battery.as_ref().filter(|b| b.wear_pct >= 20.0) {
        out.push(format!("The battery holds {:.0}% less than when new ({} of {} mWh).", b.wear_pct, b.full_mwh, b.design_mwh));
    }
    out
}

#[cfg(windows)]
pub fn read(days: u32) -> Health {
    use crate::wmi::Wmi;
    let mut h = Health::default();
    let num = |s: &str| s.trim().parse::<u64>().ok();
    match Wmi::connect("ROOT\\WMI") {
        Ok(w) => {
            let first = |wql: &str, prop: &str| w.query(wql, &[prop]).ok().and_then(|r| r.first().and_then(|x| num(&x[0])));
            let design = first("SELECT DesignedCapacity FROM BatteryStaticData", "DesignedCapacity");
            let full = first("SELECT FullChargedCapacity FROM BatteryFullChargedCapacity", "FullChargedCapacity");
            let cycles = first("SELECT CycleCount FROM BatteryCycleCount", "CycleCount").map(|c| c as u32);
            match (design, full) {
                (Some(d), Some(f)) => h.battery = battery(d as u32, f as u32, cycles),
                _ => h.unavailable.push("battery: the firmware didn't report its capacities".into()),
            }
        }
        Err(e) => h.unavailable.push(format!("battery: {e:#}")),
    }
    match Wmi::connect("ROOT\\Microsoft\\Windows\\Storage") {
        Ok(w) => {
            let names: Vec<(String, String)> = w.query("SELECT DeviceId, FriendlyName FROM MSFT_PhysicalDisk", &["DeviceId", "FriendlyName"]).unwrap_or_default().into_iter().map(|r| (r[0].clone(), r[1].clone())).collect();
            match w.query("SELECT DeviceId, Wear, Temperature, ReadErrorsTotal, PowerOnHours FROM MSFT_StorageReliabilityCounter", &["DeviceId", "Wear", "Temperature", "ReadErrorsTotal", "PowerOnHours"]) {
                Ok(rows) if !rows.is_empty() => {
                    for r in rows {
                        let name = names.iter().find(|(id, _)| *id == r[0]).map(|(_, n)| n.clone()).unwrap_or_else(|| format!("disk {}", r[0]));
                        h.disks.push(Disk { name, wear_pct: num(&r[1]).map(|v| v as u32), temp_c: num(&r[2]).filter(|t| *t > 0).map(|v| v as u32), read_errors: num(&r[3]), power_on_hours: num(&r[4]) });
                    }
                }
                Ok(_) => h.unavailable.push("disk wear: no drive answered (Windows needs admin to read it)".into()),
                Err(e) => h.unavailable.push(format!("disk wear: {e:#}")),
            }
        }
        Err(e) => h.unavailable.push(format!("disk wear: {e:#}")),
    }
    match events(days) {
        Ok(e) => h.events = Some(e),
        Err(e) => h.unavailable.push(format!("events: {e:#}")),
    }
    h.findings = findings(&h);
    h
}

/// Counts events in the System log over the last `days` days.
#[cfg(windows)]
fn events(days: u32) -> anyhow::Result<Events> {
    use windows::core::PCWSTR;
    use windows::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EVT_HANDLE};
    let ms = days as u64 * 86_400_000;
    let count = |xpath: String| -> anyhow::Result<u64> {
        let chan: Vec<u16> = "System".encode_utf16().chain(Some(0)).collect();
        let q: Vec<u16> = xpath.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let h = EvtQuery(None, PCWSTR(chan.as_ptr()), PCWSTR(q.as_ptr()), EvtQueryChannelPath.0)?;
            let mut n = 0u64;
            let mut batch = [0isize; 64];
            loop {
                let mut got = 0u32;
                if EvtNext(h, &mut batch, 2_000, 0, &mut got).is_err() || got == 0 {
                    break;
                }
                for e in &batch[..got as usize] {
                    let _ = EvtClose(EVT_HANDLE(*e));
                }
                n += got as u64;
            }
            let _ = EvtClose(h);
            Ok(n)
        }
    };
    let since = format!("TimeCreated[timediff(@SystemTime) <= {ms}]");
    Ok(Events {
        days,
        hardware_errors: count(format!("*[System[Provider[@Name='Microsoft-Windows-WHEA-Logger'] and {since}]]"))?,
        crashes: count(format!("*[System[Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and (EventID=1001) and {since}]]"))?,
        unexpected_shutdowns: count(format!("*[System[Provider[@Name='Microsoft-Windows-Kernel-Power'] and (EventID=41) and {since}]]"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_wear_from_capacities() {
        let b = battery(86_000, 52_201, Some(97)).unwrap();
        assert_eq!(b.wear_pct, 39.3);
        assert_eq!(battery(0, 52_201, None), None);
        assert_eq!(battery(86_000, 0, None), None);
        assert_eq!(battery(50_000, 200_000, None), None, "nonsense capacity");
        assert_eq!(battery(50_000, 51_000, None).unwrap().wear_pct, 0.0, "a new battery above design is 0% worn");
    }

    #[test]
    fn findings_name_what_matters() {
        let h = Health {
            battery: battery(86_000, 52_201, Some(97)),
            disks: vec![
                Disk { name: "Samsung PM9A1".into(), wear_pct: Some(0), temp_c: Some(60), read_errors: None, power_on_hours: None },
                Disk { name: "Old SSD".into(), wear_pct: Some(91), temp_c: None, read_errors: Some(3), power_on_hours: Some(27_621) },
            ],
            events: Some(Events { days: 30, hardware_errors: 2, crashes: 1, unexpected_shutdowns: 0 }),
            ..Default::default()
        };
        let f = findings(&h);
        assert_eq!(f.len(), 5, "{f:?}");
        assert!(f[0].contains("blue screen"));
        assert!(f.iter().any(|x| x.contains("Old SSD has used 91%")));
        assert!(f.iter().any(|x| x.contains("39% less")));
        assert!(!f.iter().any(|x| x.contains("Samsung")));
    }
}
