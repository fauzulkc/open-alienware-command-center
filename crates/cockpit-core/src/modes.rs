//! What the app remembers between mode and fan changes, and the emergency rule as the app (and `cockpit
//! guard`) runs it when it talks to the firmware itself. The hardware service runs its own rule.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use wiregent_oem_alienware::rules::{Emergency, EmergencyRule, RateLimit, EMERGENCY_MAX_C};

/// The last mode change, and the mode G-Mode was switched on from (the boost key goes back to it).
#[derive(Default)]
pub struct Modes {
    pub previous: Option<u8>,
    pub limit: RateLimit,
    pub last: Option<Value>,
    pub started: Option<Instant>,
    /// Fans this app raised, and to what (so it can put them back when it quits).
    pub boosted: BTreeMap<u8, u8>,
    /// The emergency rule, in direct mode (the hardware service runs its own).
    pub emergency: Option<EmergencyRule>,
}

impl Modes {
    pub fn emergency_active(&self) -> bool {
        self.emergency.as_ref().is_some_and(|r| r.active())
    }

    pub fn raised(&self) -> Raised {
        Raised { fans: self.boosted.clone(), emergency: self.emergency_active() }
    }

    /// Keeps the record of raised fans on disk (see [`Raised`]).
    pub fn persist(&self) {
        let _ = self.raised().save_to(&Raised::default_path());
    }
}

/// One reading into the emergency rule. The rule is rebuilt when the person changes the trigger, but never
/// while it is engaged: changing the setting mid-emergency must not hand the fans back early.
pub fn emergency_step(m: &mut Modes, trigger_c: i32, t_ms: u64, hottest: Option<i32>) -> Option<Emergency> {
    let wanted = trigger_c.min(EMERGENCY_MAX_C);
    if m.emergency.as_ref().is_none_or(|r| r.trigger_c() != wanted && !r.active()) {
        m.emergency = Some(EmergencyRule::new(wanted));
    }
    hottest.and_then(|h| m.emergency.as_mut().unwrap().push(t_ms, h))
}

/// The hottest valid sensor in a reader snapshot (as JSON).
pub fn hottest(snapshot: &Value) -> Option<i32> {
    snapshot["sensors"].as_array()?.iter().filter_map(|s| s["celsius"].as_i64()).max().map(|c| c as i32)
}

/// The service is the single writer when it runs; otherwise this app writes itself if it's elevated.
pub fn route_for(service: bool, admin: bool) -> Result<&'static str, String> {
    match (service, admin) {
        (true, _) => Ok("service"),
        (false, true) => Ok("direct"),
        (false, false) => Err("Switching modes needs the hardware service, or Alien Cockpit started as administrator.".into()),
    }
}

/// Records a finished switch: the mode G-Mode was entered from (the boost key goes back to it), and the result.
pub fn record_switch(m: &mut Modes, current: Option<u8>, profile: u8, change: &Value) {
    if profile == wiregent_oem_alienware::GMODE && current != Some(wiregent_oem_alienware::GMODE) {
        m.previous = current;
    }
    m.last = Some(change.clone());
}

pub const EMERGENCY_HOLD: &str = "The emergency rule has every fan at full until the laptop cools down; fan changes wait until then.";

/// What this app has raised, kept on disk while anything is raised, so a crash or a kill can't leave fans
/// raised for good: the next start puts them back.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Raised {
    pub fans: BTreeMap<u8, u8>,
    /// The emergency rule had every fan at full.
    pub emergency: bool,
}

impl Raised {
    pub fn default_path() -> PathBuf {
        dirs::config_dir().unwrap_or_default().join("Alien Cockpit OSS").join("raised.json")
    }

    pub fn is_empty(&self) -> bool {
        self.fans.is_empty() && !self.emergency
    }

    /// Written while something is raised; removed when nothing is.
    pub fn save_to(&self, p: &Path) -> std::io::Result<()> {
        if self.is_empty() {
            return match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).unwrap())?;
        std::fs::rename(&tmp, p)
    }

    pub fn load_from(p: &Path) -> Option<Raised> {
        std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Raised>(&t).ok()).filter(|r| !r.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The record of raised fans exists exactly while something is raised, so a crash leaves it behind and a
    /// clean quit doesn't.
    #[test]
    fn raised_fans_are_recorded_while_raised_and_forgotten_after() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("raised.json");
        assert_eq!(Raised::load_from(&p), None);
        let r = Raised { fans: [(0x33u8, 60u8)].into_iter().collect(), emergency: false };
        r.save_to(&p).unwrap();
        assert_eq!(Raised::load_from(&p), Some(r));
        let e = Raised { fans: Default::default(), emergency: true };
        e.save_to(&p).unwrap();
        assert_eq!(Raised::load_from(&p), Some(e), "the emergency alone is worth remembering");
        Raised::default().save_to(&p).unwrap();
        assert!(!p.exists(), "nothing raised: no record");
        Raised::default().save_to(&p).unwrap();
        std::fs::write(&p, b"garbage").unwrap();
        assert_eq!(Raised::load_from(&p), None, "an unreadable record is ignored");
    }

    #[test]
    fn modes_know_what_they_raised() {
        let mut m = Modes::default();
        m.boosted.insert(0x32, 40);
        for i in 0..=5 {
            emergency_step(&mut m, 98, i * 1_000, Some(99));
        }
        assert_eq!(m.raised(), Raised { fans: [(0x32u8, 40u8)].into_iter().collect(), emergency: true });
    }

    #[test]
    fn writes_go_through_the_service_first_then_direct_when_elevated() {
        assert_eq!(route_for(true, false), Ok("service"));
        assert_eq!(route_for(true, true), Ok("service"), "the service is the single writer when it runs");
        assert_eq!(route_for(false, true), Ok("direct"));
        assert!(route_for(false, false).unwrap_err().contains("administrator"));
    }

    /// The boost key returns to the mode G-Mode was entered from; switching G-Mode on again from G-Mode, or
    /// leaving it, doesn't overwrite that.
    #[test]
    fn the_mode_before_g_mode_is_remembered() {
        let mut m = Modes::default();
        record_switch(&mut m, Some(0xA1), 0xAB, &json!({ "after": "G-Mode" }));
        assert_eq!(m.previous, Some(0xA1));
        record_switch(&mut m, Some(0xAB), 0xAB, &json!({}));
        assert_eq!(m.previous, Some(0xA1));
        record_switch(&mut m, Some(0xAB), 0xA3, &json!({ "after": "Quiet" }));
        assert_eq!(m.previous, Some(0xA1));
        assert_eq!(m.last, Some(json!({ "after": "Quiet" })));
        record_switch(&mut m, Some(0xA3), 0xAB, &json!({}));
        assert_eq!(m.previous, Some(0xA3));
    }

    /// The rule as the app runs it: 5 s at the trigger engages, 30 s at or under trigger − 6 releases, and no
    /// reading moves nothing.
    #[test]
    fn the_emergency_rule_engages_and_releases_in_the_app() {
        let mut m = Modes::default();
        let got: Vec<_> = (0..=5).map(|i| emergency_step(&mut m, 98, i * 1_000, Some(99))).collect();
        assert_eq!(got.last(), Some(&Some(Emergency::Engage)));
        assert!(got[..5].iter().all(Option::is_none));
        assert!(m.emergency_active());
        assert_eq!(emergency_step(&mut m, 98, 6_000, None), None, "no reading: no decision");
        assert_eq!(emergency_step(&mut m, 98, 7_000, Some(92)), None);
        assert_eq!(emergency_step(&mut m, 98, 37_000, Some(92)), Some(Emergency::Release));
        assert!(!m.emergency_active());
    }

    /// A trigger changed in Settings takes effect, but never releases the fans mid-emergency.
    #[test]
    fn a_changed_trigger_applies_but_not_mid_emergency() {
        let mut m = Modes::default();
        for i in 0..=5 {
            emergency_step(&mut m, 90, i * 1_000, Some(91));
        }
        assert!(m.emergency_active(), "a lowered trigger (90) engages at 91 °C");
        assert_eq!(emergency_step(&mut m, 98, 6_000, Some(91)), None);
        assert!(m.emergency_active(), "raising the trigger while engaged doesn't drop the rule");
        assert_eq!(m.emergency.as_ref().unwrap().trigger_c(), 90);
        for i in 7..=40 {
            emergency_step(&mut m, 98, i * 1_000, Some(80));
        }
        assert!(!m.emergency_active());
        emergency_step(&mut m, 98, 41_000, Some(80));
        assert_eq!(m.emergency.as_ref().unwrap().trigger_c(), 98, "rebuilt with the new trigger once released");
        assert_eq!(emergency_step(&mut m, 120, 42_000, Some(80)), None);
        assert_eq!(m.emergency.as_ref().unwrap().trigger_c(), 98, "never above 98 °C");
    }

    #[test]
    fn the_hottest_sensor_is_found_in_a_snapshot() {
        assert_eq!(hottest(&json!({ "sensors": [{ "celsius": 63 }, { "celsius": null }, { "celsius": 100 }] })), Some(100));
        assert_eq!(hottest(&json!({ "sensors": [{ "celsius": null }] })), None);
        assert_eq!(hottest(&Value::Null), None);
    }
}
