//! Your current lighting, read from AWCC's own database (a copy, read-only). Best effort until shown on the
//! keyboard: AWCC's LED ids aren't HID indices, and what the colour's alpha byte means is unconfirmed.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Group {
    pub leds: Vec<u32>,
    pub effect: i64,
    pub brightness: i64,
    /// `#RRGGBB` from AWCC's ARGB value.
    pub colors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Device {
    pub device: String,
    pub preset: String,
    pub groups: Vec<Group>,
    /// AWCC's built-in animations for power states (e.g. "DC - Sleep").
    pub power_states: Vec<String>,
}

pub fn awcc_db() -> PathBuf {
    dirs::data_local_dir().unwrap_or_default().join(r"Alienware\Alienware Command Center\FX\FXRepository.db")
}

pub fn argb_to_hex(argb: u64) -> String {
    format!("#{:06X}", argb & 0x00FF_FFFF)
}

/// Parses one preset's `DataJson`.
pub fn parse_preset(data: &Value) -> (Vec<Group>, Vec<String>) {
    let s = &data["Static"];
    let groups = s["Animation"]["Sequence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|seq| {
            let leds: Vec<u32> = seq["LEDs"].as_array()?.iter().filter_map(|l| l.as_u64().map(|x| x as u32)).collect();
            let action = seq["Actions"].as_array()?.first()?;
            let colors = action["Color"].as_array().map(|c| c.iter().filter_map(|x| x.as_u64()).map(argb_to_hex).collect()).unwrap_or_default();
            Some(Group { leds, effect: action["Effect"].as_i64().unwrap_or(0), brightness: action["Brightness"].as_i64().unwrap_or(100), colors })
        })
        .collect();
    let power = s["PredefinedAnimations"].as_array().into_iter().flatten().filter_map(|a| a["Name"].as_str().map(String::from)).collect();
    (groups, power)
}

/// Reads the active presets from a copy of the database, so AWCC's own file is never opened for writing.
pub fn read(db: &Path) -> Result<Vec<Device>> {
    let tmp = std::env::temp_dir().join(format!("alien-cockpit-fx-{}.db", std::process::id()));
    std::fs::copy(db, &tmp).with_context(|| format!("copying {}", db.display()))?;
    let out = (|| -> Result<Vec<Device>> {
        let c = rusqlite::Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut q = c.prepare(
            "SELECT d.Model, g.Name, p.DataJson FROM ActivePresets a \
             JOIN GamePresets g ON g.PresetId = a.PresetId \
             JOIN PresetDetailInfo p ON p.PresetId = a.PresetId AND p.CapabilityId = 1 \
             JOIN DeviceInfo i ON i.DeviceInstanceId = p.DeviceInstanceId \
             JOIN Devices d ON d.DeviceId = i.DeviceId",
        )?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (device, preset, data) = row?;
            let (groups, power_states) = parse_preset(&serde_json::from_str(&data).unwrap_or(Value::Null));
            let d = Device { device, preset, groups, power_states };
            // AWCC keeps one active-preset row per preset type; the same lighting shows once.
            if !out.contains(&d) {
                out.push(d);
            }
        }
        Ok(out)
    })();
    let _ = std::fs::remove_file(&tmp);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colours_and_groups_from_a_preset() {
        assert_eq!(argb_to_hex(4294902015), "#FF00FF");
        let data = json!({ "Static": { "Animation": { "Sequence": [
            { "LEDs": [0, 13, 14], "Actions": [{ "Effect": 4, "Brightness": 100, "Color": [4294902015u64] }] },
            { "LEDs": [], "Actions": [] }
        ] }, "PredefinedAnimations": [{ "Name": "DC - Sleep" }] } });
        let (g, p) = parse_preset(&data);
        assert_eq!(g, vec![Group { leds: vec![0, 13, 14], effect: 4, brightness: 100, colors: vec!["#FF00FF".into()] }]);
        assert_eq!(p, vec!["DC - Sleep"]);
    }
}

/// Reads the real AWCC database on this machine (read-only copy). Run with `-- --ignored`.
#[cfg(test)]
#[test]
#[ignore]
fn reads_this_machines_awcc_lighting() {
    let d = read(&awcc_db()).unwrap();
    for x in &d {
        eprintln!("{} [{}]: {} group(s) {:?}; power states {:?}", x.device, x.preset, x.groups.len(), x.groups.iter().map(|g| (g.leds.len(), g.colors.clone())).collect::<Vec<_>>(), x.power_states);
    }
    assert!(!d.is_empty());
}
