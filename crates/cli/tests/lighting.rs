//! `cockpit lighting`: every command with `--dry-run`, the writes on a fake keyboard, and the refusals.
//! Every test hands the command line a `FakeIo` (fake keyboard, counting AWCC pauser, a recorded AWCC
//! preset), so nothing here can reach the real keyboard or pause the real AWCC. Settings go to a temporary
//! folder (`--config-dir`).

use cockpit_cli::{run_with_lights, Clock};
use cockpit_core::lightctl::{self, FakeIo, LightingSettings, Mode, PowerState};
use cockpit_core::Settings;
use serde_json::Value;
use std::path::Path;

#[derive(Default)]
struct FakeClock(u64);

impl Clock for FakeClock {
    fn now_ms(&mut self) -> u64 {
        self.0
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.0 += ms;
    }
}

/// `cockpit --json lighting show` on the owner's x17 R2, 2026-11-24: 11 keyboard groups covering 91 LEDs.
const SHOW_JSON: &str = r##"{"devices":[{"device":"0x0406","groups":[],"power_states":["DC - Sleep","DC - Working","DC - Low Battery","AC - Sleep","AC - Fully Charge","AC - Charging","Spectrum"],"preset":"System Default"},{"device":"0x0406_AdvCherryMechanicalKB","groups":[{"brightness":100,"colors":["#FF00FF"],"effect":4,"leds":[0,13,14,15,31,32,33,35,19,46,47,59,65,66,73,82,105,113,115]},{"brightness":100,"colors":["#2AAAFF"],"effect":4,"leds":[1,2,3,4,5,6,7,8,9,10,11,12]},{"brightness":100,"colors":["#2AAAFF"],"effect":4,"leds":[42,44,45]},{"brightness":100,"colors":["#3500FF"],"effect":4,"leds":[43,62,63,64]},{"brightness":100,"colors":["#FFCDE9"],"effect":4,"leds":[133,134,112,92,94,114,91,70,71,72,74,90,111,69,89,68,88,67,48,49,50,51,52,53,55]},{"brightness":100,"colors":["#6BFFB9"],"effect":4,"leds":[101,103,104,100,81,83,84,85,86,87]},{"brightness":100,"colors":["#6022FF"],"effect":4,"leds":[135,17,18,16]},{"brightness":100,"colors":["#FF00FF"],"effect":4,"leds":[61]},{"brightness":100,"colors":["#FF0000"],"effect":4,"leds":[40]},{"brightness":100,"colors":["#FFED87"],"effect":4,"leds":[20,21,22,23,24,25,26,27,28,29,30]},{"brightness":100,"colors":["#0000FF"],"effect":4,"leds":[109]}],"power_states":[],"preset":"System Default"}]}"##;

fn fake() -> FakeIo {
    let v: Value = serde_json::from_str(SHOW_JSON).unwrap();
    FakeIo { awcc: serde_json::from_value(v["devices"].clone()).unwrap(), ..Default::default() }
}

/// Runs `cockpit --config-dir DIR lighting …` on `io`; the exit code and what it printed.
fn cockpit(dir: &Path, io: &FakeIo, json: bool, rest: &[&str]) -> (i32, String) {
    let mut a = vec!["cockpit".to_string(), "--config-dir".into(), dir.to_string_lossy().into_owned()];
    if json {
        a.push("--json".into());
    }
    a.push("lighting".into());
    a.extend(rest.iter().map(|s| s.to_string()));
    let mut out = Vec::new();
    let code = run_with_lights(a, &mut out, &mut FakeClock::default(), None, Some(Box::new(io.clone())));
    (code, String::from_utf8(out).unwrap())
}

fn cockpit_json(dir: &Path, io: &FakeIo, rest: &[&str]) -> (i32, Value) {
    let (code, t) = cockpit(dir, io, true, rest);
    let v = serde_json::from_str(t.lines().last().unwrap_or("null")).unwrap_or_else(|e| panic!("not JSON ({e}): {t}"));
    (code, v)
}

fn lighting(dir: &Path) -> LightingSettings {
    Settings::load_from(&dir.join("settings.json")).lighting
}

fn custom_settings(dir: &Path) {
    let mut s = Settings::default();
    s.lighting.mode = Mode::Custom;
    s.save_to(&dir.join("settings.json")).unwrap();
}

fn hexes(steps: &[alienfx::Step<64>]) -> Vec<String> {
    lightctl::describe(steps)
}

#[test]
fn status_in_text_and_json() {
    let d = tempfile::tempdir().unwrap();
    let io = fake();
    let (code, t) = cockpit(d.path(), &io, false, &["status"]);
    assert_eq!(code, 0, "{t}");
    for want in ["Mode        awcc", "Keyboard    on, 100 %, static", "keyboard found", "\"System Default\", 91 key(s)"] {
        assert!(t.contains(want), "missing {want:?} in\n{t}");
    }
    let (code, v) = cockpit_json(d.path(), &io, &["status"]);
    assert_eq!(code, 0);
    assert_eq!(v["layout"].as_array().unwrap().len(), 91);
    assert_eq!(v["groups"].as_array().unwrap().len(), 4);
    assert_eq!(v["device"]["keyboard"], true);
    assert_eq!(v["device"]["zonesVisible"], false);
    assert_eq!(v["awcc"]["keys"]["40"], "#FF0000");
    assert_eq!(v["settings"]["mode"], "awcc");
    assert!(io.keyboard.calls().is_empty(), "status sends nothing");
}

/// Every command with --dry-run: exit 0, the reports a real run would send, nothing saved, nothing sent,
/// AWCC never paused.
#[test]
fn every_command_dry_runs_without_saving_or_sending() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let before = std::fs::read(d.path().join("settings.json")).unwrap();
    let io = fake();
    let base = lighting(d.path());
    type Change = Box<dyn Fn(&mut LightingSettings)>;
    let cmds: Vec<(Vec<&str>, Change)> = vec![
        (vec!["color", "#FF0000"], Box::new(|l| l.keyboard.color = "#FF0000".into())),
        (vec!["group", "wasd", "#00FF00"], Box::new(|l| {
            l.keyboard.groups.insert("WASD".into(), "#00FF00".into());
        })),
        (vec!["key", "Esc", "#0000FF"], Box::new(|l| {
            l.keyboard.keys.insert(0, "#0000FF".into());
        })),
        (vec!["key", "135", "#0000FF"], Box::new(|l| {
            l.keyboard.keys.insert(135, "#0000FF".into());
        })),
        (vec!["brightness", "40"], Box::new(|l| l.keyboard.brightness = 40)),
        (vec!["off"], Box::new(|l| l.keyboard.on = false)),
        (vec!["on"], Box::new(|l| l.keyboard.on = true)),
        (vec!["effect", "dual-wave", "--tempo", "200", "--color2", "#123456"], Box::new(|l| {
            l.keyboard.effect = lightctl::KeyEffect::DualWave;
            l.keyboard.tempo = 200;
            l.keyboard.color2 = "#123456".into();
        })),
        (vec!["effect", "static"], Box::new(|_| {})),
        (vec!["mode", "custom"], Box::new(|_| {})),
    ];
    for (args, change) in &cmds {
        let mut a = args.clone();
        a.push("--dry-run");
        let (code, v) = cockpit_json(d.path(), &io, &a);
        assert_eq!(code, 0, "{args:?}: {v}");
        let mut want = base.clone();
        change(&mut want);
        assert_eq!(v["dryRun"], true);
        assert_eq!(serde_json::from_value::<LightingSettings>(v["settings"].clone()).unwrap(), want, "{args:?}");
        let kb: Vec<String> = serde_json::from_value(v["keyboard"].clone()).unwrap();
        assert_eq!(kb, hexes(&lightctl::plan(&want, &PowerState::default()).unwrap()), "{args:?}: the exact reports");
        assert!(kb.iter().any(|r| r.starts_with("cc ")), "{args:?}");
        let (code, t) = cockpit(d.path(), &io, false, &a);
        assert_eq!(code, 0);
        assert!(t.starts_with("Dry run: nothing saved, nothing sent."), "{t}");
    }
    let (code, v) = cockpit_json(d.path(), &io, &["off", "--dry-run"]);
    assert_eq!((code, v["keyboard"].as_array().unwrap().len()), (0, 4), "off: reset, status, read, brightness 0");
    assert!(v["keyboard"][3].as_str().unwrap().starts_with("cc 83 38 9c 00"));

    let (code, v) = cockpit_json(d.path(), &io, &["mode", "awcc", "--dry-run"]);
    assert_eq!(code, 0);
    assert!(v["keyboard"].as_array().unwrap().is_empty(), "mode awcc sends nothing");

    let (code, v) = cockpit_json(d.path(), &io, &["restore-awcc", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    let look = lightctl::awcc_keys(&io.awcc).unwrap();
    let kb: Vec<String> = serde_json::from_value(v["keyboard"].clone()).unwrap();
    assert_eq!(kb, hexes(&lightctl::restore_awcc_plan(&look).unwrap()));
    assert_eq!(v["settings"]["mode"], "awcc");

    let (code, v) = cockpit_json(d.path(), &io, &["import-awcc", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["settings"]["keyboard"]["color"], "#FFCDE9");
    assert!(v["keyboard"].as_array().unwrap().is_empty(), "import never writes");

    assert_eq!(std::fs::read(d.path().join("settings.json")).unwrap(), before, "nothing saved");
    assert!(io.keyboard.calls().is_empty(), "nothing sent");
    assert_eq!((io.pauser.paused(), io.pauser.recovered()), (0, 0), "AWCC never paused or resumed");
}

/// Crash recovery: each write first resumes AWCC once (a crash may have left it paused); saves that write
/// nothing don't.
#[test]
fn each_write_first_resumes_awcc_once() {
    let d = tempfile::tempdir().unwrap();
    let io = fake();
    assert_eq!(cockpit(d.path(), &io, false, &["color", "#FF0000"]).0, 0);
    assert_eq!(io.pauser.recovered(), 0, "mode awcc: nothing written");
    assert_eq!(cockpit(d.path(), &io, false, &["mode", "custom"]).0, 0);
    assert_eq!((io.pauser.recovered(), io.pauser.paused()), (1, 1), "one resume, then the write's own pause");
    assert_eq!(cockpit(d.path(), &io, false, &["restore-awcc"]).0, 0);
    assert_eq!(io.pauser.recovered(), 2);
    assert_eq!(cockpit(d.path(), &io, false, &["import-awcc"]).0, 0);
    assert_eq!(io.pauser.recovered(), 2, "import writes nothing");
}

#[test]
fn awcc_mode_saves_but_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let io = fake();
    let (code, t) = cockpit(d.path(), &io, false, &["color", "#FF0000"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("Not written: the mode is awcc"), "{t}");
    assert_eq!(lighting(d.path()).keyboard.color, "#FF0000", "saved in the app's file");
    assert!(io.keyboard.calls().is_empty());
    assert_eq!(io.pauser.paused(), 0);
}

#[test]
fn custom_mode_writes_with_awcc_paused() {
    let d = tempfile::tempdir().unwrap();
    let io = fake();
    let (code, v) = cockpit_json(d.path(), &io, &["mode", "custom"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["applied"], true);
    let l = lighting(d.path());
    assert_eq!(l.mode, Mode::Custom);
    let want: Vec<Vec<u8>> = lightctl::plan(&l, &PowerState::default()).unwrap().iter().filter_map(|s| s.report().map(|r| r.as_bytes().to_vec())).collect();
    assert_eq!(io.keyboard.sent(), want, "byte for byte");
    assert_eq!((io.pauser.paused(), io.pauser.resumed()), (1, 1));

    io.keyboard.clear();
    let (code, t) = cockpit(d.path(), &io, false, &["key", "W", "#00FF00"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("Written:") && t.contains("report(s) sent to the keyboard"), "{t}");
    assert_eq!(lighting(d.path()).keyboard.keys.get(&43).map(String::as_str), Some("#00FF00"));
    assert!(io.keyboard.sent().iter().any(|r| r[1] == 0x8c && r[2] == 0x02 && r[4..].chunks(4).any(|b| b == [44, 0, 255, 0])), "W (43, sent as 44) in green");
    assert_eq!(io.pauser.resumed(), 2);

    let mut s = Settings::load_from(&d.path().join("settings.json"));
    s.lighting.pause_awcc = false;
    s.save_to(&d.path().join("settings.json")).unwrap();
    assert_eq!(cockpit(d.path(), &io, false, &["off"]).0, 0);
    assert_eq!(io.pauser.paused(), 2, "pauseAwcc off: not paused");
    assert_eq!(io.keyboard.sent().last().unwrap()[1..5], [0x83, 0x38, 0x9c, 0]);
}

#[test]
fn restore_and_import() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let io = fake();
    let (code, v) = cockpit_json(d.path(), &io, &["import-awcc"]);
    assert_eq!(code, 0, "{v}");
    let l = lighting(d.path());
    assert_eq!((l.mode, l.keyboard.color.as_str(), l.keyboard.keys.len()), (Mode::Custom, "#FFCDE9", 66), "seeded per key, mode kept");
    assert!(io.keyboard.calls().is_empty(), "import writes nothing");

    let (code, t) = cockpit(d.path(), &io, false, &["restore-awcc"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("\"System Default\" is back on the keyboard (91 keys)"), "{t}");
    assert_eq!(lighting(d.path()).mode, Mode::Awcc, "lighting is AWCC's again");
    let look = lightctl::awcc_keys(&io.awcc).unwrap();
    let want: Vec<Vec<u8>> = lightctl::restore_awcc_plan(&look).unwrap().iter().filter_map(|s| s.report().map(|r| r.as_bytes().to_vec())).collect();
    assert_eq!(io.keyboard.sent(), want);
    assert_eq!((io.pauser.paused(), io.pauser.resumed()), (1, 1));

    let empty = FakeIo::default();
    let (code, t) = cockpit(d.path(), &empty, false, &["restore-awcc"]);
    assert_eq!(code, 1, "{t}");
    assert!(t.contains("no active keyboard preset"), "{t}");
}

#[test]
fn refusals_exit_2_and_change_nothing() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let before = std::fs::read(d.path().join("settings.json")).unwrap();
    let io = fake();
    for (args, why) in [
        (&["brightness", "150"][..], "0–100"),
        (&["color", "red"], "#RRGGBB"),
        (&["color", "#12345"], "#RRGGBB"),
        (&["key", "34", "#FFFFFF"], "isn't a key"),
        (&["key", "nope", "#FFFFFF"], "isn't a key"),
        (&["key", "Esc", "blue"], "#RRGGBB"),
        (&["group", "Arrows", "#FFFFFF"], "isn't a key group"),
        (&["effect", "spectrum"], "isn't an effect"),
        (&["effect", "wave", "--tempo", "300"], "0–255"),
        (&["effect", "wave", "--color2", "#xyzxyz"], "#RRGGBB"),
        (&["mode", "rainbow"], "invalid value"),
        (&["brightness", "lots"], "invalid value"),
    ] {
        let (code, t) = cockpit(d.path(), &io, false, args);
        assert_eq!(code, 2, "{args:?}: {t}");
        assert!(t.contains(why), "{args:?}: {t}");
        let (code, _) = cockpit(d.path(), &io, false, &[args, &["--dry-run"]].concat());
        assert_eq!(code, 2, "{args:?} --dry-run");
    }
    let (code, v) = cockpit_json(d.path(), &io, &["brightness", "101"]);
    assert_eq!((code, v["refused"].as_bool()), (2, Some(true)));
    assert_eq!(std::fs::read(d.path().join("settings.json")).unwrap(), before, "nothing saved");
    assert!(io.keyboard.calls().is_empty(), "nothing sent");
    assert_eq!(io.pauser.paused(), 0);
}

#[test]
fn a_failed_write_is_exit_1_and_awcc_is_resumed() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let io = fake();
    io.keyboard.fail_at(2);
    let (code, t) = cockpit(d.path(), &io, false, &["color", "#FF0000"]);
    assert_eq!(code, 1, "{t}");
    assert!(t.contains("saved to") && t.contains("writing the keyboard failed"), "{t}");
    assert_eq!((io.pauser.paused(), io.pauser.resumed()), (1, 1), "AWCC resumed after the failure");

    let io = fake();
    io.pauser.fail();
    let (code, t) = cockpit(d.path(), &io, false, &["color", "#00FF00"]);
    assert_eq!(code, 1, "{t}");
    assert!(t.contains("couldn't pause AWCC") && t.contains("nothing was sent"), "{t}");
    assert!(io.keyboard.calls().is_empty());
}

/// Without a test's devices, writes need Windows: on Linux every change is exit 2 and saves nothing.
#[cfg(not(windows))]
#[test]
fn writes_are_windows_only() {
    let d = tempfile::tempdir().unwrap();
    let run = |rest: &[&str]| {
        let mut a = vec!["cockpit".to_string(), "--config-dir".into(), d.path().to_string_lossy().into_owned(), "lighting".into()];
        a.extend(rest.iter().map(|s| s.to_string()));
        let mut out = Vec::new();
        let code = cockpit_cli::run_with(a, &mut out, &mut FakeClock::default(), None);
        (code, String::from_utf8(out).unwrap())
    };
    for cmd in [&["color", "#FF0000"][..], &["mode", "custom"], &["off"], &["restore-awcc"], &["import-awcc"], &["restore-awcc", "--dry-run"]] {
        let (code, t) = run(cmd);
        assert_eq!(code, 2, "{cmd:?}: {t}");
        assert!(t.contains("Windows only"), "{t}");
    }
    assert!(!d.path().join("settings.json").exists(), "nothing saved");
    assert_eq!(run(&["color", "#FF0000", "--dry-run"]).0, 0, "a dry run writes nothing, so it runs anywhere");
    assert_eq!(run(&["status"]).0, 0);
}

/// `lighting reactive --dry-run`: scripted typing on a fake keyboard inside the command; nothing saved, nothing
/// sent to the devices given, AWCC never paused. Runs anywhere.
#[test]
fn reactive_dry_run_prints_report_counts() {
    let d = tempfile::tempdir().unwrap();
    let io = fake();
    let (code, v) = cockpit_json(d.path(), &io, &["reactive", "--dry-run", "--seconds", "12"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dryRun"], true);
    let sum = &v["summary"];
    assert_eq!(sum["stats"]["first"], 11, "the full frame: reset, status, 7 colorSets, loop, update");
    let per: Vec<u64> = sum["perSecond"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap()).collect();
    assert!(per.len() >= 11 && per[1..7].iter().all(|n| *n > 0 && *n <= 18 * 6), "typing: some reports every second, at most 18 frames of 6: {per:?}");
    assert!(sum["stats"]["maxFrameReports"].as_u64().unwrap() <= 6, "one or two colorSets a frame");
    assert_eq!(sum["restore"]["keyboard"], 11, "stopping writes the static colours back");
    assert!(io.keyboard.calls().is_empty() && io.pauser.paused() == 0, "the devices given are never touched");
    assert!(!d.path().join("settings.json").exists(), "nothing saved");
    let (code, t) = cockpit(d.path(), &io, false, &["reactive", "--dry-run"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("Dry run: nothing saved, nothing sent") && t.contains("reports a second while typing"), "{t}");
}

/// The foreground run on the fake keyboard: the full frame with AWCC paused, nothing more without key
/// presses (a test's devices get no key listener), the static colours back after --seconds, AWCC resumed.
#[cfg(windows)]
#[test]
fn reactive_runs_and_restores_on_the_fake_keyboard() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let before = std::fs::read_to_string(d.path().join("settings.json")).unwrap();
    let io = fake();
    let (code, t) = cockpit(d.path(), &io, false, &["reactive", "--seconds", "2"]);
    assert_eq!(code, 0, "{t}");
    assert!(t.contains("Reactive glow on") && t.contains("static colours are back"), "{t}");
    let l = lighting(d.path());
    let want: Vec<Vec<u8>> = lightctl::plan(&l, &PowerState::default()).unwrap().iter().filter_map(|s| s.report().map(|r| r.as_bytes().to_vec())).collect();
    assert_eq!(io.keyboard.sent(), [want.clone(), want].concat(), "the full frame, then the static write back");
    assert_eq!((io.pauser.recovered(), io.pauser.paused(), io.pauser.resumed()), (1, 1, 1), "AWCC paused once for the whole run");
    assert_eq!(std::fs::read_to_string(d.path().join("settings.json")).unwrap(), before, "the settings are used, not changed");

    // AWCC mode or a hardware effect: refused, nothing sent.
    let d2 = tempfile::tempdir().unwrap();
    let io2 = fake();
    let (code, t) = cockpit(d2.path(), &io2, false, &["reactive", "--seconds", "1"]);
    assert_eq!(code, 2, "{t}");
    assert!(t.contains("mode awcc"), "{t}");
    let mut s = Settings::default();
    s.lighting.mode = Mode::Custom;
    s.lighting.keyboard.effect = lightctl::KeyEffect::Wave;
    s.save_to(&d2.path().join("settings.json")).unwrap();
    let (code, t) = cockpit(d2.path(), &io2, false, &["reactive", "--seconds", "1"]);
    assert_eq!(code, 2, "{t}");
    assert!(t.contains("Static"), "{t}");
    assert!(io2.keyboard.calls().is_empty());

    // A write that fails: exit 1, the static colours written back, AWCC resumed.
    let io3 = fake();
    io3.keyboard.fail_at(0);
    let (code, t) = cockpit(d.path(), &io3, false, &["reactive", "--seconds", "1"]);
    assert_eq!(code, 1, "{t}");
    assert!(t.contains("couldn't start"), "{t}");
    assert_eq!((io3.pauser.paused(), io3.pauser.resumed()), (1, 1));
}

/// On Linux the glow can't read keys or write the keyboard: exit 2, with the reason.
#[cfg(not(windows))]
#[test]
fn reactive_is_windows_only() {
    let d = tempfile::tempdir().unwrap();
    custom_settings(d.path());
    let (code, t) = cockpit(d.path(), &fake(), false, &["reactive", "--seconds", "1"]);
    assert_eq!(code, 2, "{t}");
    assert!(t.contains("Windows only") && t.contains("Raw Input"), "{t}");
}
