//! Every `cockpit` command on the demo backend (the recorded x17 R2 capture), in text and in JSON, plus the
//! refusals. Settings and logs go to a temporary folder (`--config-dir`), never the app's own.

use cockpit_cli::{parse_mode, run, run_with, Clock};
use serde_json::Value;
use std::path::Path;
use wiregent_hw::machines::ReplayMachine;
use wiregent_hw::replay::Replay;

/// A clock that doesn't sleep: time moves only when a command waits. `stop_after` sleeps, it acts as Ctrl+C.
#[derive(Default)]
struct FakeClock {
    t: u64,
    sleeps: u32,
    stop_after: Option<u32>,
}

impl Clock for FakeClock {
    fn now_ms(&mut self) -> u64 {
        self.t
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.t += ms;
        self.sleeps += 1;
    }
    fn stopped(&self) -> bool {
        self.stop_after.is_some_and(|n| self.sleeps >= n)
    }
}

fn args<'a>(dir: &'a Path, rest: &[&'a str]) -> Vec<String> {
    let mut a = vec!["cockpit".to_string(), "--backend".into(), "demo".into(), "--config-dir".into(), dir.to_string_lossy().into_owned()];
    a.extend(rest.iter().map(|s| s.to_string()));
    a
}

/// Runs on the demo backend; returns the exit code and what it printed.
fn cockpit(dir: &Path, rest: &[&str]) -> (i32, String) {
    let mut out = Vec::new();
    let code = run_with(args(dir, rest), &mut out, &mut FakeClock::default(), None);
    (code, String::from_utf8(out).unwrap())
}

/// The same with --json: every printed line is one JSON document.
fn cockpit_json(dir: &Path, rest: &[&str]) -> (i32, Vec<Value>) {
    let mut a = vec!["--json"];
    a.extend_from_slice(rest);
    let (code, text) = cockpit(dir, &a);
    (code, text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect())
}

/// On a machine the test keeps a handle to (to see what reached the firmware).
fn on_replay(dir: &Path, fw: &Replay, clock: &mut FakeClock, rest: &[&str]) -> (i32, String) {
    let mut out = Vec::new();
    let m = ReplayMachine::new(fw.clone()).unwrap();
    let code = run_with(args(dir, rest), &mut out, clock, Some(Box::new(m)));
    (code, String::from_utf8(out).unwrap())
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn info_in_text_and_json() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["info"]);
    assert_eq!(code, 0, "{t}");
    for want in ["Backend   replay", "Model     Alienware x17 R2", "BIOS      1.32.0", "Verified  yes", "Quiet (0xA3)", "G-Mode (0xAB)", "CPU zone 1 (0x32)", "GPU zone 2 (0x3C)"] {
        assert!(t.contains(want), "missing {want:?} in\n{t}");
    }
    let (code, v) = cockpit_json(d.path(), &["info"]);
    assert_eq!(code, 0);
    let v = &v[0];
    assert_eq!((v["backend"].as_str(), v["verified"].as_bool()), (Some("replay"), Some(true)));
    assert_eq!(v["profiles"].as_array().unwrap().iter().map(|p| p["arg"].as_str().unwrap()).collect::<Vec<_>>(), vec!["quiet", "balanced", "performance", "full-speed", "battery-saver", "g-mode"]);
    assert_eq!(v["fans"][2]["zone"], "CPU zone 2");
    assert_eq!(v["fans"][2]["hex"], "0x3B");
}

#[test]
fn status_in_text_and_json() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["status"]);
    assert_eq!(code, 0, "{t}");
    for want in ["Mode        G-Mode (0xAB)", "CPU         100 °C", "GPU         63 °C", "CPU zone 1  0x32   4600 RPM  100% of 4200 RPM max", "CPU zone 2  0x3B      0 RPM", "Alerts      none"] {
        assert!(t.contains(want), "missing {want:?} in\n{t}");
    }
    let (code, v) = cockpit_json(d.path(), &["status"]);
    assert_eq!((code, v.len()), (0, 1));
    let s = &v[0];
    assert_eq!(s["mode"]["name"], "G-Mode");
    assert_eq!(s["sensors"][0]["celsius"], 100);
    assert_eq!(s["fans"].as_array().unwrap().iter().map(|f| f["zone"].as_str().unwrap()).collect::<Vec<_>>(), vec!["CPU zone 1", "GPU zone 1", "CPU zone 2", "GPU zone 2"]);
    assert_eq!(s["fans"][0]["percent"], 100);
    assert_eq!(s["alerts"], serde_json::json!([]));
}

/// Watching long enough on the capture (fan 0x3B at 0 RPM at 100 °C while 0x32 spins) raises the app's
/// fan-fault alert, in the app's words.
#[test]
fn status_watch_repeats_and_reports_alerts_like_the_app() {
    let d = tmp();
    let (code, v) = cockpit_json(d.path(), &["status", "--watch", "2", "--count", "3"]);
    assert_eq!((code, v.len()), (0, 3));
    assert_eq!(v[2]["t_ms"], 4_000, "one reading every 2 s");
    let (code, v) = cockpit_json(d.path(), &["status", "--watch", "2", "--count", "70"]);
    assert_eq!(code, 0);
    let alerts: Vec<&Value> = v.iter().flat_map(|s| s["alerts"].as_array().unwrap()).collect();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(alerts[0]["kind"], "fan-fault");
    assert_eq!(alerts[0]["text"], "Fan 0x3B stood still while its sensor was hot and its partner spun: likely a fan fault.");
    let (_, t) = cockpit(d.path(), &["status", "--watch", "2", "--count", "70"]);
    assert!(t.contains("Alert: Fan 0x3B stood still while its sensor was hot and its partner spun: likely a fan fault."), "{t}");
}

#[test]
fn modes_in_text_and_json() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["modes"]);
    assert_eq!(code, 0);
    assert!(t.contains("* G-Mode        0xAB  g-mode"), "{t}");
    assert!(t.contains("  Balanced      0xA0  balanced"), "{t}");
    let (_, v) = cockpit_json(d.path(), &["modes"]);
    assert_eq!(v[0]["current"]["hex"], "0xAB");
    assert_eq!(v[0]["modes"].as_array().unwrap().iter().filter(|m| m["current"] == true).count(), 1);
}

#[test]
fn mode_switches_through_the_gates() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["mode", "balanced"]);
    assert_eq!((code, t.trim()), (0, "Balanced (0xA0): switched from G-Mode."));
    let (code, t) = cockpit(d.path(), &["mode", "g-mode"]);
    assert_eq!((code, t.trim()), (0, "Already G-Mode (0xAB): nothing written."));
    let (code, v) = cockpit_json(d.path(), &["mode", "0xA4"]);
    assert_eq!(code, 0);
    assert_eq!((v[0]["after"].as_str(), v[0]["confirmed"].as_bool(), v[0]["id"].as_str()), (Some("Full Speed"), Some(true), Some("0xA4")));
}

#[test]
fn mode_names_and_ids_parse() {
    assert_eq!(parse_mode("full-speed"), Some(0xA4));
    assert_eq!(parse_mode("G-Mode"), Some(0xAB));
    assert_eq!(parse_mode("battery_saver"), Some(0xA5));
    assert_eq!(parse_mode("0xA0"), Some(0xA0));
    assert_eq!(parse_mode("turbo"), None);
}

#[test]
fn unknown_or_unreported_modes_are_refused_with_exit_2() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["mode", "turbo"]);
    assert_eq!(code, 2);
    assert!(t.starts_with("refused: unknown mode 'turbo'"), "{t}");
    let (code, v) = cockpit_json(d.path(), &["mode", "turbo"]);
    assert_eq!((code, v[0]["refused"].as_bool()), (2, Some(true)));
    let (code, t) = cockpit(d.path(), &["mode", "0xA2"]);
    assert_eq!(code, 2);
    assert!(t.contains("Cool (0xA2) isn't a mode this laptop reported"), "{t}");
}

#[test]
fn fans_in_text_and_json() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["fans"]);
    assert_eq!(code, 0);
    assert_eq!(t.lines().count(), 4, "{t}");
    assert!(t.lines().next().unwrap().starts_with("CPU zone 1  0x32   4600 RPM"), "{t}");
    let (_, v) = cockpit_json(d.path(), &["fans"]);
    assert_eq!(v[0]["fans"][1]["zone"], "GPU zone 1");
    assert_eq!(v[0]["fans"][1]["rpm"], v[0]["fans"][1]["rpm"].as_u64().unwrap());
}

#[test]
fn fan_boost_one_a_zone_or_all() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["fan", "boost", "0x33", "60"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.starts_with("GPU zone 1 (0x33): boost 0 → 60, confirmed"), "{t}");
    let (code, v) = cockpit_json(d.path(), &["fan", "boost", "cpu", "40"]);
    assert_eq!(code, 0);
    assert_eq!(v[0]["changes"].as_array().unwrap().iter().map(|c| c["zone"].as_str().unwrap()).collect::<Vec<_>>(), vec!["CPU zone 1", "CPU zone 2"]);
    let (code, v) = cockpit_json(d.path(), &["fan", "boost", "all", "100"]);
    assert_eq!((code, v[0]["changes"].as_array().unwrap().len()), (0, 4));
    let (code, v) = cockpit_json(d.path(), &["fan", "boost", "gpu", "0"]);
    assert_eq!((code, v[0]["changes"][1]["fan"].as_str()), (0, Some("0x3C")));
}

#[test]
fn fan_reset_in_text_and_json() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["fan", "reset"]);
    assert_eq!((code, t.trim()), (0, "Every fan is back on the firmware's own curve."));
    let (code, v) = cockpit_json(d.path(), &["fan", "reset"]);
    assert_eq!((code, v[0]["reset"].as_array().unwrap().len()), (0, 4));
}

/// Boost 101, a fan or mode the laptop didn't report, an unknown fan name: exit 2 and nothing written.
#[test]
fn refusals_exit_2_and_write_nothing() {
    let d = tmp();
    let fw = Replay::october();
    for (cmd, want) in [(&["fan", "boost", "0x32", "101"][..], "overboost"), (&["fan", "boost", "0x3D", "50"], "isn't one this laptop reported"), (&["fan", "boost", "sideways", "10"], "isn't a fan"), (&["mode", "turbo"], "unknown mode"), (&["mode", "0xA2"], "isn't a mode this laptop reported")] {
        let (code, t) = on_replay(d.path(), &fw, &mut FakeClock::default(), cmd);
        assert_eq!(code, 2, "{cmd:?}: {t}");
        assert!(t.contains(want), "{cmd:?}: {t}");
    }
    assert!(fw.writes().is_empty(), "{:X?}", fw.writes());
}

/// The gates' own refusals (here the rate limit inside `control`) are exit 2 as well.
#[test]
fn a_gate_refusal_from_the_machine_is_exit_2() {
    let d = tmp();
    let fw = Replay::october();
    let mut m = ReplayMachine::new(fw.clone()).unwrap();
    use wiregent_hw::machine::Machine;
    for (i, level) in [10u8, 20, 30, 40, 50].iter().enumerate() {
        m.set_fan_boost(0x33, *level, i as u64).unwrap();
    }
    let mut out = Vec::new();
    let code = run_with(args(d.path(), &["fan", "boost", "0x33", "60"]), &mut out, &mut FakeClock { t: 6, ..Default::default() }, Some(Box::new(m)));
    let t = String::from_utf8(out).unwrap();
    assert_eq!(code, 2, "{t}");
    assert!(t.contains("too many changes"), "{t}");
    assert_eq!(fw.writes().len(), 5);
}

/// The emergency rule as the app runs it: 5 s at 100 °C engages every fan to full; stopping puts them back.
#[test]
fn guard_engages_and_puts_the_fans_back() {
    let d = tmp();
    let fw = Replay::october();
    let (code, t) = on_replay(d.path(), &fw, &mut FakeClock::default(), &["guard", "--count", "8"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.starts_with("Guarding on the replay backend: every fan to full after 5 s at 98 °C, back after 30 s at 92 °C or cooler."), "{t}");
    assert!(t.contains("100 °C for 5 seconds: every fan is at full until the laptop cools to 92 °C."), "{t}");
    assert!(t.trim_end().ends_with("Stopped: every fan is back on the firmware's own curve."), "{t}");
    let w = fw.writes();
    assert_eq!(w.len(), 8, "four fans to full, then four back: {w:X?}");
    assert!(w[..4].iter().all(|x| x >> 16 == 100), "{w:X?}");
    assert!(w[4..].iter().all(|x| x >> 16 == 0), "{w:X?}");
}

#[test]
fn guard_in_json_and_its_trigger_from_the_settings() {
    let d = tmp();
    assert_eq!(cockpit(d.path(), &["settings", "set", "emergencyC=90"]).0, 0);
    let (code, v) = cockpit_json(d.path(), &["guard", "--count", "8"]);
    assert_eq!(code, 0);
    let events: Vec<&str> = v.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(events, vec!["start", "engage", "stop"]);
    assert_eq!((v[0]["triggerC"].as_i64(), v[0]["releaseC"].as_i64()), (Some(90), Some(84)));
    assert_eq!(v[1]["hottestC"], 100);
    assert_eq!(v[2]["errors"], serde_json::json!([]));
}

/// Ctrl+C ends the loop, and the fans still go back.
#[test]
fn guard_stops_on_ctrl_c_and_resets() {
    let d = tmp();
    let fw = Replay::october();
    let mut clock = FakeClock { stop_after: Some(7), ..Default::default() };
    let (code, t) = on_replay(d.path(), &fw, &mut clock, &["guard"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("every fan is at full"), "{t}");
    assert!(t.contains("Stopped: every fan is back"), "{t}");
    assert!(fw.writes().iter().rev().take(4).all(|x| x >> 16 == 0));
}

#[test]
fn guard_refuses_a_trigger_above_98() {
    let d = tmp();
    let fw = Replay::october();
    let (code, t) = on_replay(d.path(), &fw, &mut FakeClock::default(), &["guard", "--trigger", "99", "--count", "1"]);
    assert_eq!(code, 2);
    assert!(t.contains("it can only be lowered"), "{t}");
    assert!(fw.writes().is_empty());
}

fn write_log(dir: &Path) {
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let p = |t: u64, c: i64| serde_json::json!({ "t": t, "at": 1_762_696_980_000u64 + t, "cpu_c": c, "gpu_c": 60, "cpu_w": 24.6, "fans": [4600, 1200, 0, 3000], "profile": "Balanced" }).to_string();
    std::fs::write(logs.join("2026-11-09.jsonl"), format!("{}\n{}\n", p(0, 96), p(30_000, 97))).unwrap();
    std::fs::write(logs.join("2020-01-01.jsonl"), "{}\n").unwrap();
}

#[test]
fn log_days_read_and_cleanup() {
    let d = tmp();
    write_log(d.path());
    let (code, t) = cockpit(d.path(), &["log", "days"]);
    assert_eq!(code, 0);
    assert!(t.starts_with("2026-11-09  1 KB\n2020-01-01  1 KB\n"), "{t}");
    let (_, v) = cockpit_json(d.path(), &["log", "days"]);
    assert_eq!(v[0]["days"][0]["day"], "2026-11-09");
    let (code, t) = cockpit(d.path(), &["log", "read", "2026-11-09"]);
    assert_eq!(code, 0);
    assert_eq!(t.lines().next().unwrap(), "2025-11-09 14:03:00 UTC  Balanced  CPU 96 °C  GPU 60 °C  25 W  fans 4600/1200/0/3000 RPM");
    let (_, v) = cockpit_json(d.path(), &["log", "read", "2026-11-09", "--max", "1"]);
    assert_eq!(v[0]["points"].as_array().unwrap().len(), 1);
    let (code, t) = cockpit(d.path(), &["log", "read", "2026-11-10"]);
    assert_eq!((code, t.trim()), (0, "Nothing logged on 2026-11-10."));
    let (code, t) = cockpit(d.path(), &["log", "read", "../secret"]);
    assert_eq!(code, 2, "{t}");
    let (code, v) = cockpit_json(d.path(), &["log", "cleanup"]);
    assert_eq!(code, 0);
    assert!(v[0]["removed"].as_array().unwrap().iter().any(|x| x == "2020-01-01"), "{v:?}");
    let (code, t) = cockpit(d.path(), &["log", "cleanup"]);
    assert_eq!(code, 0);
    assert!(t.starts_with("Nothing to remove (keeping 14 days"), "{t}");
}

#[test]
fn settings_get_and_set_use_the_apps_file_and_checks() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["settings", "get"]);
    assert_eq!(code, 0);
    assert!(t.contains("emergencyC = 98\n") && t.contains("log.everyS = 30\n") && t.contains("theme = \"dark\"\n"), "{t}");
    let (_, v) = cockpit_json(d.path(), &["settings", "get"]);
    assert_eq!(v[0]["emergencyC"], 98);
    let (code, t) = cockpit(d.path(), &["settings", "set", "emergencyC=90"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.starts_with("emergencyC = 90\nSaved to "), "{t}");
    assert_eq!(cockpit_core::Settings::load_from(&d.path().join("settings.json")).emergency_c, 90, "the app's own file and format");
    let (_, v) = cockpit_json(d.path(), &["settings", "get", "emergencyC"]);
    assert_eq!(v[0], serde_json::json!({ "key": "emergencyC", "value": 90 }));
    let (code, v) = cockpit_json(d.path(), &["settings", "set", "gameModes.warframe=0xA1"]);
    assert_eq!((code, v[0]["value"].as_str()), (0, Some("0xA1")));
    for (bad, want) in [("emergencyC=99", "85–98"), ("autostart=true", "app's Settings"), ("nonsense=1", "no setting"), ("theme", "isn't key=value"), ("defaultProfile=0xAB", "default mode")] {
        let (code, t) = cockpit(d.path(), &["settings", "set", bad]);
        assert_eq!(code, 2, "{bad}: {t}");
        assert!(t.contains(want), "{bad}: {t}");
    }
    assert_eq!(cockpit(d.path(), &["settings", "get", "nope"]).0, 2);
    assert_eq!(cockpit_core::Settings::load_from(&d.path().join("settings.json")).emergency_c, 90, "refusals change nothing");
}

#[test]
fn boost_key_target_follows_the_settings() {
    let d = tmp();
    let (code, t) = cockpit(d.path(), &["boost-key", "target", "g-mode"]);
    assert_eq!(code, 0);
    assert_eq!(t.trim(), "Boost key (awcc: toggles G-Mode, back to the mode it left or your default): from G-Mode (0xAB) it switches to Balanced (0xA0).");
    let (_, v) = cockpit_json(d.path(), &["boost-key", "target", "g-mode", "--previous", "performance"]);
    assert_eq!(v[0]["target"]["hex"], "0xA1");
    let (_, v) = cockpit_json(d.path(), &["boost-key", "target", "balanced"]);
    assert_eq!(v[0]["target"]["name"], "G-Mode");
    cockpit(d.path(), &["settings", "set", "boostKey=cycle"]);
    let (_, v) = cockpit_json(d.path(), &["boost-key", "target", "quiet"]);
    assert_eq!((v[0]["behaviour"].as_str(), v[0]["target"]["name"].as_str()), (Some("cycle"), Some("Balanced")));
    let (_, v) = cockpit_json(d.path(), &["boost-key", "target", "none"]);
    assert_eq!(v[0]["target"]["name"], "Balanced");
    assert_eq!(cockpit(d.path(), &["boost-key", "target", "turbo"]).0, 2);
}

#[test]
fn usage_help_and_version() {
    let d = tmp();
    let mut out = Vec::new();
    assert_eq!(run(["cockpit", "--help"], &mut out), 0);
    let help = String::from_utf8(out).unwrap();
    for cmd in ["info", "status", "modes", "mode", "fans", "fan", "guard", "health", "drift", "lighting", "games", "log", "settings", "boost-key"] {
        assert!(help.contains(&format!("  {cmd} ")), "{cmd} missing from\n{help}");
    }
    assert_eq!(run(["cockpit", "--version"], &mut Vec::new()), 0);
    assert_eq!(run(["cockpit"], &mut Vec::new()), 2, "no command");
    assert_eq!(cockpit(d.path(), &["fan", "boost", "0x32", "-5"]).0, 2, "not a number");
    assert_eq!(run(["cockpit", "--backend", "bogus", "info"], &mut Vec::new()), 2);
    assert_eq!(run(["cockpit", "--backend", "demo", "guard", "--max", "3"], &mut Vec::new()), 2, "guard's limit is --count, like status");
}

/// The real entry point (`run`, real clock) on the demo backend: ok is 0, refusals are 2.
#[test]
fn run_maps_exit_codes() {
    let d = tmp();
    let dir = d.path().to_string_lossy().into_owned();
    let via_run = |rest: &[&str]| {
        let mut a = vec!["cockpit", "--backend", "demo", "--config-dir", dir.as_str()];
        a.extend_from_slice(rest);
        let mut out = Vec::new();
        (run(a, &mut out), String::from_utf8(out).unwrap())
    };
    assert_eq!(via_run(&["info"]).0, 0);
    assert_eq!(via_run(&["mode", "turbo"]).0, 2);
    assert_eq!(via_run(&["fan", "boost", "all", "101"]).0, 2);
    assert_eq!(via_run(&["settings", "set", "theme=neon"]).0, 2);
    let (code, t) = via_run(&["guard", "--count", "1"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.trim_end().ends_with("Stopped: every fan is back on the firmware's own curve."), "{t}");
}

/// A command or backend this system doesn't have: exit 2, saying so.
#[test]
fn unsupported_here_is_exit_2() {
    #[cfg(windows)]
    {
        let mut out = Vec::new();
        assert_eq!(run(["cockpit", "--backend", "linux", "info"], &mut out), 2);
        assert!(String::from_utf8(out).unwrap().contains("Linux only"));
    }
    #[cfg(not(windows))]
    {
        let d = tmp();
        for cmd in [&["health"][..], &["drift"], &["lighting", "probe"], &["lighting", "show"], &["games"]] {
            let (code, t) = cockpit(d.path(), cmd);
            assert_eq!(code, 2, "{cmd:?}: {t}");
            assert!(t.contains("Windows only"), "{cmd:?}: {t}");
        }
        let mut out = Vec::new();
        assert_eq!(run(["cockpit", "--backend", "service", "info"], &mut out), 2);
    }
}

/// Windows' own records, read-only: each answers in text and JSON (exit 1 only where this machine lacks
/// the thing, e.g. no AWCC lighting database).
#[cfg(windows)]
#[test]
fn windows_records_answer_in_text_and_json() {
    let d = tmp();
    for cmd in [&["drift"][..], &["games"], &["lighting", "probe"], &["lighting", "show"], &["health", "--days", "1"]] {
        let (code, t) = cockpit(d.path(), cmd);
        assert!(code == 0 || (code == 1 && t.starts_with("error: ")), "{cmd:?}: {code} {t}");
        let (code, v) = cockpit_json(d.path(), cmd);
        assert!(code == 0 || v[0]["error"].is_string(), "{cmd:?}: {v:?}");
    }
    let (_, v) = cockpit_json(d.path(), &["drift"]);
    assert!(v[0]["ok"].is_boolean(), "{v:?}");
}
