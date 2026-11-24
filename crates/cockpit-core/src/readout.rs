//! How a reading is put into words, the same way in the app's notifications, its window
//! (`app/ui/src/logic.ts` mirrors these) and the command line.

use serde_json::Value;
use wiregent_hw::reader::{sensor_name, FanReading};
use wiregent_oem_alienware::rules::Emergency;

/// The person-facing text of a reader alert (as JSON).
pub fn alert_text(a: &Value) -> String {
    match a["kind"].as_str() {
        Some("fan-fault") => format!("Fan 0x{:02X} stood still while its sensor was hot and its partner spun: likely a fan fault.", a["fan"].as_u64().unwrap_or(0)),
        Some("stuck-reading") => format!("Fan 0x{:02X} reports a speed that never changes ({} RPM): its reading isn't a real measurement.", a["fan"].as_u64().unwrap_or(0), a["rpm"].as_u64().unwrap_or(0)),
        Some("zone-stopped") => format!("Every {} fan reads 0 RPM while it is hot: nothing is cooling it. Ease the load and have the fans checked.", if a["sensor"].as_u64() == Some(6) { "GPU" } else { "CPU" }),
        Some("sensor-stale") => format!("No valid {} temperature for 30 s or more.", a["name"].as_str().unwrap_or("sensor")),
        _ => a.to_string(),
    }
}

/// What the emergency rule just did, in the words the person sees (always shown, whatever the notification
/// setting: this is the one message that must reach them). `errors` are fans that didn't take it.
pub fn emergency_text(e: Emergency, hot: Option<i32>, release_c: i32, errors: &[String]) -> String {
    let text = match e {
        Emergency::Engage => format!("{} °C for 5 seconds: every fan is at full until the laptop cools to {release_c} °C.", hot.unwrap_or_default()),
        Emergency::Release => "Cooled down: the fans are back to your setting.".to_string(),
    };
    if errors.is_empty() {
        text
    } else {
        format!("{text} Some fans didn't take it: {}", errors.join("; "))
    }
}

/// AWCC's names for the fans: CPU zone 1, CPU zone 2, GPU zone 1… numbered per sensor in id order.
pub fn zone_name(fans: &[FanReading], fan: &FanReading) -> String {
    let mut same: Vec<u8> = fans.iter().filter(|x| x.sensor == fan.sensor).map(|x| x.id).collect();
    same.sort_unstable();
    let n = same.iter().position(|id| *id == fan.id).map_or(0, |i| i + 1);
    let sensor = fan.sensor.map(sensor_name).unwrap_or_else(|| "—".into());
    format!("{sensor} zone {n}")
}

/// The fan's speed as a share of the maximum it reports (AWCC's "zone speed"), 0–100.
pub fn percent_of_max(rpm: Option<u32>, max: Option<u32>) -> Option<u32> {
    match (rpm, max) {
        (Some(r), Some(m)) if m > 0 => Some(((r as f64 / m as f64) * 100.0).round().min(100.0) as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn alert_texts_match_the_reader_alerts() {
        assert!(alert_text(&json!({ "kind": "fan-fault", "fan": 59 })).contains("0x3B"));
        assert!(alert_text(&json!({ "kind": "stuck-reading", "fan": 60, "rpm": 3000 })).contains("3000 RPM"));
        assert!(alert_text(&json!({ "kind": "sensor-stale", "name": "CPU" })).contains("CPU"));
        assert!(alert_text(&json!({ "kind": "zone-stopped", "sensor": 1, "fans": [50, 59] })).starts_with("Every CPU fan reads 0 RPM"));
    }

    #[test]
    fn emergency_texts() {
        assert_eq!(emergency_text(Emergency::Engage, Some(99), 92, &[]), "99 °C for 5 seconds: every fan is at full until the laptop cools to 92 °C.");
        assert!(emergency_text(Emergency::Release, None, 92, &["0x32: no".into()]).ends_with("Some fans didn't take it: 0x32: no"));
    }

    /// The same names the window uses (logic.test.ts: CPU zone 1, GPU zone 1, CPU zone 2, GPU zone 2).
    #[test]
    fn fans_are_named_by_zone_like_the_app() {
        let f = |id, sensor| FanReading { id, sensor: Some(sensor), rpm: None, max_rpm: None };
        let fans = vec![f(0x32, 1), f(0x33, 6), f(0x3B, 1), f(0x3C, 6)];
        assert_eq!(fans.iter().map(|x| zone_name(&fans, x)).collect::<Vec<_>>(), vec!["CPU zone 1", "GPU zone 1", "CPU zone 2", "GPU zone 2"]);
        assert_eq!(percent_of_max(Some(4600), Some(4200)), Some(100), "capped like the window");
        assert_eq!(percent_of_max(Some(2100), Some(4200)), Some(50));
        assert_eq!(percent_of_max(Some(2100), None), None);
        assert_eq!(percent_of_max(Some(0), Some(0)), None);
    }
}
