//! The person's choices, one file shared by the app and the command line
//! (`<config dir>/Alien Cockpit OSS/settings.json`). Validated on every save; safety limits can only be
//! tightened.

use crate::log;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The person's choices. Validated on every save; safety limits can only be tightened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Show the October capture when no live source is available (clearly labelled as demo data).
    pub demo: bool,
    /// The emergency fan rule's trigger, °C: 85..=98, never higher.
    pub emergency_c: i32,
    /// The mode to return to when G-Mode is toggled off (AWCC's behaviour), as "0xA0".
    pub default_profile: String,
    /// "awcc" (toggle G-Mode like AWCC), "cycle" (step through modes).
    pub boost_key: String,
    /// Per fan id ("0x32"): curve points [°C, boost %] for the custom profile.
    pub fan_curves: std::collections::BTreeMap<String, Vec<[i32; 2]>>,
    /// Per game id: the mode to switch to while it runs.
    pub game_modes: std::collections::BTreeMap<String, String>,
    pub notify: bool,
    /// "dark" (default), "gray" or "light".
    pub theme: String,
    /// The daily log: how often, how long, how big.
    pub log: log::LogSettings,
    /// Look and feel: each can be turned off.
    pub effects: Effects,
    /// Without the hardware service, restart as administrator at start (one UAC prompt) for live readings.
    pub live_admin: bool,
    /// Start in the tray at sign-in, as administrator without a prompt (a Task Scheduler task). Off by default.
    pub autostart: bool,
}

/// Interface effects, each on its own switch (Settings → Appearance).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Effects {
    /// Retired 2026-11-09 (the owner asked for no pointer-driven movement); kept so old settings files load.
    pub motion: bool,
    /// A colour change on controls under the pointer; nothing moves or glows.
    pub hover: bool,
    /// A press and ripple when clicking.
    pub press: bool,
    /// Larger text throughout.
    pub large_text: bool,
}

impl Default for Effects {
    fn default() -> Self {
        Effects { motion: false, hover: true, press: true, large_text: true }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { demo: false, emergency_c: 98, default_profile: "0xA0".into(), boost_key: "awcc".into(), fan_curves: Default::default(), game_modes: Default::default(), notify: true, theme: "dark".into(), log: log::LogSettings::default(), effects: Effects::default(), live_admin: true, autostart: false }
    }
}

pub const PROFILES: &[&str] = &["0xA3", "0xA0", "0xA1", "0xA4", "0xA5", "0xAB"];

/// Settings keys whose children are free-form (a fan id, a game id) rather than fixed fields.
const MAPS: &[&str] = &["fanCurves", "gameModes"];

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(85..=98).contains(&self.emergency_c) {
            return Err("the emergency trigger must be 85–98 °C".into());
        }
        if !PROFILES.contains(&self.default_profile.as_str()) || self.default_profile == "0xAB" {
            return Err("the default mode must be one of Quiet, Balanced, Performance, Full Speed or Battery Saver".into());
        }
        if !["awcc", "cycle"].contains(&self.boost_key.as_str()) {
            return Err("unknown boost-key behaviour".into());
        }
        for (fan, pts) in &self.fan_curves {
            if !fan.starts_with("0x") || pts.len() < 2 || pts.len() > 8 {
                return Err(format!("fan {fan}: a curve has 2–8 points"));
            }
            if pts.windows(2).any(|w| w[1][0] <= w[0][0]) || pts.iter().any(|p| !(20..=105).contains(&p[0]) || !(0..=100).contains(&p[1])) {
                return Err(format!("fan {fan}: points must rise in temperature (20–105 °C) with boost 0–100 %"));
            }
        }
        self.log.validate()?;
        if !["dark", "gray", "light"].contains(&self.theme.as_str()) {
            return Err("the theme must be dark, gray or light".into());
        }
        if self.game_modes.values().any(|m| !PROFILES.contains(&m.as_str())) {
            return Err("a game is set to an unknown mode".into());
        }
        Ok(())
    }

    /// Where the app and the command line keep the settings.
    pub fn default_path() -> PathBuf {
        dirs::config_dir().unwrap_or_default().join("Alien Cockpit OSS").join("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::default_path())
    }

    /// Missing, unreadable or invalid files give the defaults (the safe ones); fields this version doesn't
    /// know are ignored and missing ones take their defaults, so older and newer files still load.
    pub fn load_from(p: &Path) -> Self {
        std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Settings>(&t).ok()).filter(|s| s.validate().is_ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::default_path())
    }

    /// Written to a temporary file and renamed over the old one, so a crash mid-save can't leave half a file
    /// (which would load as defaults and lose every choice).
    pub fn save_to(&self, p: &Path) -> Result<(), String> {
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }

    /// One setting by its key as the settings file spells it, dotted for nested ones ("emergencyC",
    /// "log.everyS", "fanCurves.0x32"). None for a key that doesn't exist.
    pub fn get(&self, key: &str) -> Option<Value> {
        let mut v = serde_json::to_value(self).ok()?;
        for part in key.split('.') {
            v = v.get(part)?.clone();
        }
        Some(v)
    }

    /// These settings with one key changed, validated. `value` is JSON when it parses as JSON ("90", "true",
    /// "[[50,0],[90,100]]"), otherwise a string ("0xA1", "light"). Under fanCurves and gameModes a new id
    /// adds an entry and `null` removes one; anywhere else the key must already exist.
    pub fn with(&self, key: &str, value: &str) -> Result<Settings, String> {
        let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        let mut root = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let parts: Vec<&str> = key.split('.').collect();
        let (last, parents) = parts.split_last().ok_or("no key")?;
        let mut at = &mut root;
        for p in parents {
            at = at.get_mut(*p).filter(|v| v.is_object()).ok_or_else(|| format!("no setting '{key}'"))?;
        }
        let in_map = parents.len() == 1 && MAPS.contains(&parents[0]);
        let obj = at.as_object_mut().ok_or_else(|| format!("no setting '{key}'"))?;
        if in_map && parsed.is_null() {
            obj.remove(*last);
        } else if in_map || obj.contains_key(*last) {
            obj.insert(last.to_string(), parsed);
        } else {
            return Err(format!("no setting '{key}'"));
        }
        let s: Settings = serde_json::from_value(root).map_err(|e| format!("'{value}' isn't a value '{key}' takes: {e}"))?;
        s.validate()?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_refuse_unsafe_or_unknown_values() {
        let ok = Settings::default();
        assert!(ok.validate().is_ok());
        let bad = |f: &dyn Fn(&mut Settings)| {
            let mut s = Settings::default();
            f(&mut s);
            s.validate().is_err()
        };
        assert!(bad(&|s| s.emergency_c = 99), "can't raise the emergency trigger past 98 °C");
        assert!(bad(&|s| s.emergency_c = 70));
        assert!(bad(&|s| s.default_profile = "0xAB".into()), "G-Mode isn't a resting mode");
        assert!(bad(&|s| s.default_profile = "0xA2".into()));
        assert!(bad(&|s| s.boost_key = "macro".into()));
        assert!(bad(&|s| { s.fan_curves.insert("0x32".into(), vec![[60, 20], [50, 40]]); }), "falling temperature");
        assert!(bad(&|s| { s.fan_curves.insert("0x32".into(), vec![[60, 20], [80, 140]]); }), "boost over 100 %");
        assert!(bad(&|s| { s.game_modes.insert("g".into(), "0x99".into()); }));
        assert!(bad(&|s| s.theme = "neon".into()));
        assert_eq!(Settings::default().theme, "dark", "dark by default");
        let mut good = Settings::default();
        good.fan_curves.insert("0x32".into(), vec![[50, 0], [70, 40], [90, 100]]);
        good.game_modes.insert("warframe".into(), "0xA1".into());
        assert!(good.validate().is_ok());
    }

    #[test]
    fn settings_round_trip_and_fall_back_to_safe_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("settings.json");
        assert_eq!(Settings::load_from(&p), Settings::default(), "missing file");
        let mut s = Settings::default();
        s.emergency_c = 90;
        s.default_profile = "0xA1".into();
        s.game_modes.insert("warframe".into(), "0xAB".into());
        s.save_to(&p).unwrap();
        assert_eq!(Settings::load_from(&p), s, "round trip");
        assert!(!p.with_extension("json.tmp").exists(), "no temporary file left behind");
        std::fs::write(&p, b"{ not json").unwrap();
        assert_eq!(Settings::load_from(&p), Settings::default(), "corrupt file");
        std::fs::write(&p, br#"{ "emergencyC": 120 }"#).unwrap();
        assert_eq!(Settings::load_from(&p), Settings::default(), "an unsafe trigger is never loaded");
        std::fs::write(&p, br#"{ "emergencyC": 88, "someFutureSetting": true }"#).unwrap();
        let l = Settings::load_from(&p);
        assert_eq!((l.emergency_c, l.theme.as_str()), (88, "dark"), "unknown fields ignored, missing ones defaulted");
    }

    /// The command line's `settings get` / `settings set key=value`: same keys as the file, same validation.
    #[test]
    fn one_key_at_a_time() {
        let s = Settings::default();
        assert_eq!(s.get("emergencyC"), Some(Value::from(98)));
        assert_eq!(s.get("log.everyS"), Some(Value::from(30)));
        assert_eq!(s.get("nope"), None);
        assert_eq!(s.with("emergencyC", "90").unwrap().emergency_c, 90);
        assert_eq!(s.with("defaultProfile", "0xA1").unwrap().default_profile, "0xA1", "a bare string");
        assert_eq!(s.with("log.everyS", "60").unwrap().log.every_s, 60);
        assert!(!s.with("effects.hover", "false").unwrap().effects.hover);
        let c = s.with("fanCurves.0x32", "[[50,0],[90,100]]").unwrap();
        assert_eq!(c.fan_curves["0x32"], vec![[50, 0], [90, 100]]);
        assert!(c.with("fanCurves.0x32", "null").unwrap().fan_curves.is_empty(), "null removes a map entry");
        assert_eq!(s.with("gameModes.warframe", "0xAB").unwrap().game_modes["warframe"], "0xAB");
        assert!(s.with("emergencyC", "99").unwrap_err().contains("85–98"), "validated");
        assert!(s.with("emergencyC", "hot").is_err(), "wrong type");
        assert!(s.with("someFutureSetting", "1").unwrap_err().contains("no setting"));
        assert!(s.with("log.nope", "1").is_err());
        assert!(s.with("theme.x", "1").is_err(), "not an object");
        assert!(s.with("gameModes.warframe", "0x99").is_err());
    }
}
