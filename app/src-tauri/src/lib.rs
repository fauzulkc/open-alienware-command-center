//! Alien Cockpit OSS: a standalone app for the Alienware x17 R2 that replaces AWCC and SupportAssist's everyday
//! jobs (docs/plans/x17-standalone-app.md). It talks to the firmware through the hardware service, or
//! directly when it runs as admin. Thermal modes can be switched (gated, confirmed by read-back); fan boost,
//! curves and lighting writes stay locked, with the reason, until they pass their tests.

mod autostart;
mod boostkey;
mod elevate;
mod games;
mod lighting;
mod log;
mod processes;
mod source;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_notification::NotificationExt;

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

    fn path() -> std::path::PathBuf {
        dirs::config_dir().unwrap_or_default().join("Alien Cockpit OSS").join("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// Missing, unreadable or invalid files give the defaults (the safe ones); fields this version doesn't
    /// know are ignored and missing ones take their defaults, so older and newer files still load.
    pub fn load_from(p: &std::path::Path) -> Self {
        std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Settings>(&t).ok()).filter(|s| s.validate().is_ok()).unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }

    /// Written to a temporary file and renamed over the old one, so a crash mid-save can't leave half a file
    /// (which would load as defaults and lose every choice).
    pub fn save_to(&self, p: &std::path::Path) -> Result<(), String> {
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }
}

/// The last mode change, and the mode G-Mode was switched on from (the boost key goes back to it).
#[derive(Default)]
pub struct Modes {
    previous: Option<u8>,
    limit: wiregent_oem_alienware::rules::RateLimit,
    last: Option<Value>,
    started: Option<Instant>,
    /// Fans this app raised, and to what (so it can put them back when it quits).
    boosted: std::collections::BTreeMap<u8, u8>,
    /// The emergency rule, in direct mode (the hardware service runs its own).
    emergency: Option<wiregent_oem_alienware::rules::EmergencyRule>,
}

impl Modes {
    fn emergency_active(&self) -> bool {
        self.emergency.as_ref().is_some_and(|r| r.active())
    }
}

/// One reading into the emergency rule. The rule is rebuilt when the person changes the trigger, but never
/// while it is engaged: changing the setting mid-emergency must not hand the fans back early.
pub fn emergency_step(m: &mut Modes, trigger_c: i32, t_ms: u64, hottest: Option<i32>) -> Option<wiregent_oem_alienware::rules::Emergency> {
    use wiregent_oem_alienware::rules::{EmergencyRule, EMERGENCY_MAX_C};
    let wanted = trigger_c.min(EMERGENCY_MAX_C);
    if m.emergency.as_ref().is_none_or(|r| r.trigger_c() != wanted && !r.active()) {
        m.emergency = Some(EmergencyRule::new(wanted));
    }
    hottest.and_then(|h| m.emergency.as_mut().unwrap().push(t_ms, h))
}

/// The hottest valid sensor in a reader snapshot.
fn hottest(snapshot: &Value) -> Option<i32> {
    snapshot["sensors"].as_array()?.iter().filter_map(|s| s["celsius"].as_i64()).max().map(|c| c as i32)
}

/// Carries out an emergency decision in direct mode and says so (always, whatever the notification
/// setting: this is the one message that must reach the person).
fn run_emergency(app: &AppHandle, e: wiregent_oem_alienware::rules::Emergency, person: &std::collections::BTreeMap<u8, u8>, hot: Option<i32>, release_c: i32) {
    use wiregent_oem_alienware::rules::Emergency;
    let errors = (|| -> Result<Vec<String>, String> {
        let fw = wiregent_hw::wmi::Awcc::open().map_err(|e| format!("{e:#}"))?;
        let d = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()).map_err(|e| e.to_string())?;
        Ok(wiregent_hw::control::apply_emergency(&fw, &d, e, person, wiregent_hw::control::this_machine_verified()))
    })()
    .unwrap_or_else(|e| vec![e]);
    let mut text = match e {
        Emergency::Engage => format!("{} °C for 5 seconds: every fan is at full until the laptop cools to {release_c} °C.", hot.unwrap_or_default()),
        Emergency::Release => "Cooled down: the fans are back to your setting.".to_string(),
    };
    if !errors.is_empty() {
        text = format!("{text} Some fans didn't take it: {}", errors.join("; "));
    }
    let _ = app.notification().builder().title("Alien Cockpit OSS: emergency fan rule").body(&text).show();
}

pub struct App {
    readings: source::Shared,
    modes: Arc<Mutex<Modes>>,
    demo: Arc<Mutex<bool>>,
    settings: Mutex<Settings>,
    health: Mutex<Option<(Instant, Value)>>,
    status_item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

// Commands the window polls run off the main thread (async): a sync command runs on the thread that also
// moves, minimizes and maximizes the window, so a slow one makes the title bar lag.
#[tauri::command]
async fn status(app: State<'_, App>) -> Result<Value, String> {
    let st = app.readings.lock().unwrap();
    Ok(json!({ "source": st.source, "snapshot": st.snapshot, "lists": st.lists, "power": st.power, "error": st.error, "ageMs": st.updated.map(|u| u.elapsed().as_millis() as u64), "energySaver": energy_saver(), "elevated": elevate::is_elevated() }))
}

/// Whether Windows Energy Saver (battery saver) is on: Windows then caps CPU power, so thermal modes
/// change less than they say. AWCC greys its modes out while it's on; Alien Cockpit warns instead.
/// None when Windows doesn't say.
fn energy_saver() -> Option<bool> {
    #[cfg(windows)]
    {
        let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut s) }.ok()?;
        Some(s.SystemStatusFlag == 1)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Restarts Alien Cockpit as administrator for live readings; this copy exits once the new one is started.
#[tauri::command]
fn restart_elevated(app: AppHandle) -> Result<(), String> {
    elevate::relaunch()?;
    app.exit(0);
    Ok(())
}

/// The chart history: everything, or only the points after `after` (the newest `t` the window already has),
/// so the once-a-second poll carries one or two points instead of all 600.
#[tauri::command]
async fn history(app: State<'_, App>, after: Option<u64>) -> Result<Value, String> {
    let st = app.readings.lock().unwrap();
    Ok(serde_json::to_value(points_after(&st.history, after)).unwrap_or_default())
}

pub fn points_after(history: &std::collections::VecDeque<source::Point>, after: Option<u64>) -> Vec<&source::Point> {
    match after {
        None => history.iter().collect(),
        Some(t) => history.iter().filter(|p| p.t > t).collect(),
    }
}

#[tauri::command]
async fn health(app: State<'_, App>) -> Result<Value, String> {
    if let Some((at, v)) = app.health.lock().unwrap().clone() {
        if at.elapsed() < Duration::from_secs(60) {
            return Ok(v);
        }
    }
    let v = tauri::async_runtime::spawn_blocking(|| serde_json::to_value(wiregent_hw::health::read(30)).unwrap_or_default()).await.map_err(|e| e.to_string())?;
    *app.health.lock().unwrap() = Some((Instant::now(), v.clone()));
    Ok(v)
}

#[tauri::command]
async fn drift() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        use wiregent_hw::drift::{check, live, Rules};
        let d = check(&Rules::builtin(), &live());
        json!({ "ok": d.iter().all(|x| x.severity == "info"), "drift": d })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn lighting() -> Value {
    match lighting::read(&lighting::awcc_db()) {
        Ok(devices) => json!({ "devices": devices }),
        Err(e) => json!({ "devices": [], "error": format!("{e:#}") }),
    }
}

#[tauri::command]
async fn game_library() -> Value {
    serde_json::to_value(games::read()).unwrap_or_default()
}

#[tauri::command]
async fn settings_get(app: State<'_, App>) -> Result<Settings, String> {
    Ok(app.settings.lock().unwrap().clone())
}

#[tauri::command]
async fn settings_set(app: State<'_, App>, settings: Settings) -> Result<Settings, String> {
    settings.validate()?;
    let before = app.settings.lock().unwrap().autostart;
    if settings.autostart != before {
        autostart::set(settings.autostart)?;
    }
    settings.save()?;
    *app.demo.lock().unwrap() = settings.demo;
    *app.settings.lock().unwrap() = settings.clone();
    Ok(settings)
}

/// Whether a mode switch could go through right now, and how.
fn mode_route() -> Result<&'static str, String> {
    route_for(wiregent_hw::client::service_present(), wiregent_hw::is_admin())
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
fn record_switch(m: &mut Modes, current: Option<u8>, profile: u8, change: &Value) {
    if profile == wiregent_oem_alienware::GMODE && current != Some(wiregent_oem_alienware::GMODE) {
        m.previous = current;
    }
    m.last = Some(change.clone());
}

/// Switches the thermal mode: through the service's single writer, or directly when this app is elevated.
/// Remembers the mode G-Mode was switched on from, for the boost key.
pub fn switch_mode(modes: &Mutex<Modes>, readings: &source::Shared, profile: u8) -> Result<Value, String> {
    use wiregent_hw::control::{parse_profile, set_profile, this_machine_verified};
    let current = readings.lock().unwrap().snapshot.as_ref().and_then(|s| s["profile"].as_u64()).map(|p| p as u8);
    let change = match mode_route()? {
        "service" => {
            use wiregent_hw::proto::{Request, Response, PIPE_NAME};
            match wiregent_hw::client::request(PIPE_NAME, &Request::SetProfile { profile: format!("0x{profile:02X}") }, wiregent_hw::client::SERVICE_IMAGE).map_err(|e| format!("{e:#}"))? {
                Response::ModeChanged { change } => change,
                Response::Error { message } => return Err(message),
                other => return Err(format!("unexpected answer: {other:?}")),
            }
        }
        _ => {
            let fw = wiregent_hw::wmi::Awcc::open().map_err(|e| format!("{e:#}"))?;
            let d = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()).map_err(|e| e.to_string())?;
            let mut m = modes.lock().unwrap();
            let t = m.started.get_or_insert_with(Instant::now).elapsed().as_millis() as u64;
            let r = set_profile(&fw, &d, profile, this_machine_verified(), &mut m.limit, t, &std::thread::sleep).map_err(|e| format!("{e:#}"))?;
            serde_json::to_value(r).unwrap_or_default()
        }
    };
    let _ = parse_profile;
    record_switch(&mut modes.lock().unwrap(), current, profile, &change);
    Ok(change)
}

#[tauri::command]
async fn set_mode(app: State<'_, App>, profile: String) -> Result<Value, String> {
    let p = wiregent_hw::control::parse_profile(&profile).ok_or_else(|| format!("'{profile}' isn't a mode id"))?;
    let (modes, readings) = (app.modes.clone(), app.readings.clone());
    tauri::async_runtime::spawn_blocking(move || switch_mode(&modes, &readings, p)).await.map_err(|e| e.to_string())?
}

pub const EMERGENCY_HOLD: &str = "The emergency rule has every fan at full until the laptop cools down; fan changes wait until then.";

/// Raises (or resets) one fan's boost, through the service or directly (as with modes). Held back while the
/// emergency rule has the fans.
pub fn switch_fan_boost(modes: &Mutex<Modes>, fan: u8, level: u8) -> Result<Value, String> {
    if modes.lock().unwrap().emergency_active() {
        return Err(EMERGENCY_HOLD.into());
    }
    write_fan_boost(modes, fan, level)
}

fn write_fan_boost(modes: &Mutex<Modes>, fan: u8, level: u8) -> Result<Value, String> {
    use wiregent_hw::control::{set_fan_boost, this_machine_verified};
    let change = match mode_route()? {
        "service" => {
            use wiregent_hw::proto::{Request, Response, PIPE_NAME};
            match wiregent_hw::client::request(PIPE_NAME, &Request::SetFanBoost { fan: format!("0x{fan:02X}"), level }, wiregent_hw::client::SERVICE_IMAGE).map_err(|e| format!("{e:#}"))? {
                Response::BoostChanged { change } => change,
                Response::Error { message } => return Err(message),
                other => return Err(format!("unexpected answer: {other:?}")),
            }
        }
        _ => {
            let fw = wiregent_hw::wmi::Awcc::open().map_err(|e| format!("{e:#}"))?;
            let d = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()).map_err(|e| e.to_string())?;
            let mut m = modes.lock().unwrap();
            let t = m.started.get_or_insert_with(Instant::now).elapsed().as_millis() as u64;
            serde_json::to_value(set_fan_boost(&fw, &d, fan, level, this_machine_verified(), &mut m.limit, t, &std::thread::sleep).map_err(|e| format!("{e:#}"))?).unwrap_or_default()
        }
    };
    let mut m = modes.lock().unwrap();
    if level == 0 {
        m.boosted.remove(&fan);
    } else {
        m.boosted.insert(fan, level);
    }
    m.persist();
    Ok(change)
}

/// What this app has raised, kept on disk while anything is raised, so a crash or a kill can't leave fans
/// raised for good: the next start puts them back (see `recover_raised`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Raised {
    pub fans: std::collections::BTreeMap<u8, u8>,
    /// The emergency rule had every fan at full.
    pub emergency: bool,
}

impl Raised {
    fn path() -> std::path::PathBuf {
        dirs::config_dir().unwrap_or_default().join("Alien Cockpit OSS").join("raised.json")
    }

    pub fn is_empty(&self) -> bool {
        self.fans.is_empty() && !self.emergency
    }

    /// Written while something is raised; removed when nothing is.
    pub fn save_to(&self, p: &std::path::Path) -> std::io::Result<()> {
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

    pub fn load_from(p: &std::path::Path) -> Option<Raised> {
        std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Raised>(&t).ok()).filter(|r| !r.is_empty())
    }
}

impl Modes {
    fn raised(&self) -> Raised {
        Raised { fans: self.boosted.clone(), emergency: self.emergency_active() }
    }

    fn persist(&self) {
        let _ = self.raised().save_to(&Raised::path());
    }
}

/// At start: fans a previous run raised and never put back (it crashed or was killed) go back to the
/// firmware's curve. The emergency rule re-engages within 5 s if it's still needed. Returns what was done.
fn recover_raised(modes: &Mutex<Modes>) -> Option<String> {
    let left = Raised::load_from(&Raised::path())?;
    let errors: Vec<String> = if left.emergency && mode_route() == Ok("direct") {
        (|| -> Result<Vec<String>, String> {
            let fw = wiregent_hw::wmi::Awcc::open().map_err(|e| format!("{e:#}"))?;
            let d = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()).map_err(|e| e.to_string())?;
            Ok(wiregent_hw::control::reset_fans(&fw, &d))
        })()
        .unwrap_or_else(|e| vec![e])
    } else {
        left.fans.keys().filter_map(|f| write_fan_boost(modes, *f, 0).err()).collect()
    };
    if errors.is_empty() {
        let _ = Raised::default().save_to(&Raised::path());
        Some("Fans raised before Alien Cockpit last stopped unexpectedly are back on the firmware's own curve.".into())
    } else {
        Some(format!("Fans raised before Alien Cockpit last stopped unexpectedly couldn't all be put back: {}", errors.join("; ")))
    }
}

/// Raises every listed fan to `level` (0 puts them back on the firmware's curve).
#[tauri::command]
async fn set_fan_boost(app: State<'_, App>, fans: Vec<String>, level: u8) -> Result<Vec<Value>, String> {
    let ids: Vec<u8> = fans.iter().map(|f| wiregent_hw::control::parse_profile(f).ok_or_else(|| format!("'{f}' isn't a fan id"))).collect::<Result<_, _>>()?;
    let modes = app.modes.clone();
    tauri::async_runtime::spawn_blocking(move || ids.into_iter().map(|f| switch_fan_boost(&modes, f, level)).collect::<Result<Vec<_>, _>>()).await.map_err(|e| e.to_string())?
}

/// Every fan this app raised goes back to the firmware's own curve (on quit). If the emergency rule has the
/// fans, all of them go back: nothing would be left running to release them later.
fn reset_boost(modes: &Mutex<Modes>) {
    if modes.lock().unwrap().emergency_active() && mode_route() == Ok("direct") {
        if let Ok(fw) = wiregent_hw::wmi::Awcc::open() {
            if let Ok(d) = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()) {
                if wiregent_hw::control::reset_fans(&fw, &d).is_empty() {
                    modes.lock().unwrap().boosted.clear();
                }
            }
        }
        return;
    }
    let fans: Vec<u8> = modes.lock().unwrap().boosted.keys().copied().collect();
    for f in fans {
        let _ = write_fan_boost(modes, f, 0);
    }
}

/// On quit, after `reset_boost`: nothing is raised any more, whatever the emergency rule's last state was.
fn forget_raised(modes: &Mutex<Modes>) {
    if modes.lock().unwrap().boosted.is_empty() {
        let _ = Raised::default().save_to(&Raised::path());
    }
}

#[tauri::command]
async fn log_days() -> Value {
    let days = log::days(&log::dir());
    let total: u64 = days.iter().map(|d| d.bytes).sum();
    json!({ "days": days, "totalBytes": total, "dir": log::dir() })
}

#[tauri::command]
async fn log_read(day: String) -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(move || log::read(&log::dir(), &day, 1_440)).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn log_cleanup(app: State<'_, App>) -> Result<Vec<String>, String> {
    let s = app.settings.lock().unwrap().log.clone();
    Ok(log::cleanup(&log::dir(), &log::today(), &s))
}

/// What each control can do right now, and why not when it can't.
#[tauri::command]
async fn controls(app: State<'_, App>) -> Result<Value, String> {
    let st = app.readings.lock().unwrap();
    let mut fan_notes = vec!["Boost only adds to the firmware's own curve, so it can't make a fan slower; it goes back to 0 when Alien Cockpit quits. Custom curves stay saved-only until the boost has been checked on this laptop.".to_string()];
    let hot_at_low_power = st.history.iter().rev().take(60).filter(|p| p.cpu_c.is_some_and(|c| c >= 95) && p.cpu_w.is_some_and(|w| w <= 35.0)).count();
    if hot_at_low_power >= 10 {
        fan_notes.push("The CPU runs at 95 °C or more at 35 W or less: more fan helps, but the cause is likely hardware (a fan, fins or paste).".into());
    }
    let route = mode_route();
    let modes_ok = route.is_ok() && st.source != Some(source::Source::Demo);
    let mode_reasons: Vec<String> = match (&route, st.source) {
        (_, Some(source::Source::Demo)) => vec!["Demo data: modes switch on the live laptop only.".into()],
        (Err(e), _) => vec![e.clone()],
        _ => vec![],
    };
    let m = app.modes.lock().unwrap();
    let emergency = m.emergency_active();
    let mut fan_reasons = if modes_ok { Vec::new() } else { mode_reasons.clone() };
    if emergency {
        fan_reasons.push(EMERGENCY_HOLD.into());
    }
    Ok(json!({
        "emergency": { "active": emergency, "triggerC": m.emergency.as_ref().map(|r| r.trigger_c()), "releaseC": m.emergency.as_ref().map(|r| r.release_c) },
        "modes": { "enabled": modes_ok, "route": route.ok(), "reasons": mode_reasons, "last": m.last },
        "fans": { "enabled": modes_ok && !emergency, "reasons": fan_reasons, "notes": fan_notes, "boosted": m.boosted.iter().map(|(f, l)| (format!("0x{f:02X}"), *l)).collect::<std::collections::BTreeMap<_, _>>() },
        "lighting": { "enabled": false, "reasons": ["Writing lighting needs the lighting protocol port and a test on this keyboard. The firmware keeps the current colours."] },
    }))
}

#[tauri::command]
fn open_alienware_update() -> Result<(), String> {
    std::process::Command::new("explorer.exe").arg(r"shell:AppsFolder\DellInc.AlienwareUpdate_htrsf667h5kn2!App").spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens the window, creating it again if it was closed (closing frees the web view: ~230 MB measured).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    if let Some(cfg) = app.config().app.windows.first() {
        match tauri::WebviewWindowBuilder::from_config(app, cfg).and_then(|b| b.build()) {
            Ok(w) => {
                let _ = w.set_focus();
            }
            Err(e) => eprintln!("can't open the window: {e}"),
        }
    }
}

fn tray_line(st: &source::State) -> String {
    let Some(s) = &st.snapshot else { return "No readings: install the hardware service".into() };
    let mode = s["profile_name"].as_str().unwrap_or("?");
    let cpu = s["sensors"].as_array().and_then(|a| a.iter().find(|x| x["name"] == "CPU")).and_then(|x| x["celsius"].as_i64()).map(|c| format!("CPU {c} °C")).unwrap_or_default();
    let w = st.power.as_ref().and_then(|p| p["cpuPackageW"].as_f64()).map(|w| format!(" · {w:.0} W")).unwrap_or_default();
    let demo = if st.source == Some(source::Source::Demo) { " (demo)" } else { "" };
    format!("{mode} · {cpu}{w}{demo}")
}

fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    let line = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Open Alien Cockpit", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&line, &PredefinedMenuItem::separator(app)?, &show, &quit])?;
    *app.state::<App>().status_item.lock().unwrap() = Some(line);
    let mut b = TrayIconBuilder::with_id("x17").tooltip("Alien Cockpit OSS").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.on_menu_event(|app, ev| match ev.id().as_ref() {
        "show" => show_main(app),
        "quit" => app.exit(0),
        _ => {}
    })
    .on_tray_icon_event(|tray, ev| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
            show_main(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = Settings::load();
    let args: Vec<String> = std::env::args().collect();
    let restarted = elevate::after_pid(&args);
    if let Some(pid) = restarted {
        elevate::wait_for(pid, 10);
    }
    #[cfg(windows)]
    if elevate::should_ask(settings.live_admin, elevate::is_elevated(), wiregent_hw::client::service_present(), restarted.is_some()) && elevate::relaunch().is_ok() {
        return;
    }
    let readings: source::Shared = Arc::new(Mutex::new(source::State::default()));
    let demo = Arc::new(Mutex::new(settings.demo));
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .manage(App { readings: readings.clone(), modes: Arc::new(Mutex::new(Modes::default())), demo: demo.clone(), settings: Mutex::new(settings), health: Mutex::new(None), status_item: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![status, history, health, drift, lighting, game_library, settings_get, settings_set, controls, set_mode, set_fan_boost, log_days, log_read, log_cleanup, open_alienware_update, restart_elevated])
        .setup(move |app| {
            if let Err(e) = install_tray(app.handle()) {
                eprintln!("tray icon unavailable: {e}");
            }
            // Started by the sign-in task: tray only. Closing the window frees its web view; the tray reopens it.
            if autostart::started_at_sign_in(&args) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.close();
                }
            }
            let handle = app.handle().clone();
            let (r, d) = (readings.clone(), demo.clone());
            std::thread::spawn(move || {
                let (h, h2) = (handle.clone(), handle.clone());
                let recovered = std::sync::atomic::AtomicBool::new(false);
                source::run(
                    r,
                    d,
                    move |text| {
                        if h.state::<App>().settings.lock().unwrap().notify {
                            let _ = h.notification().builder().title("Alien Cockpit OSS").body(&text).show();
                        }
                    },
                    // The emergency rule, when this app talks to the firmware itself. The hardware service runs
                    // its own; demo data never writes.
                    move |src, t, snapshot| {
                        // Once, as soon as the firmware is reachable: undo what a crashed run left raised.
                        if matches!(src, source::Source::Direct | source::Source::Service) && !recovered.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            let (modes, h3) = (h2.state::<App>().modes.clone(), h2.clone());
                            std::thread::spawn(move || {
                                if let Some(text) = recover_raised(&modes) {
                                    let _ = h3.notification().builder().title("Alien Cockpit OSS").body(&text).show();
                                }
                            });
                        }
                        if src != source::Source::Direct {
                            return;
                        }
                        let app = h2.state::<App>();
                        let trigger = app.settings.lock().unwrap().emergency_c;
                        let hot = hottest(snapshot);
                        let (decision, person, release_c) = {
                            let mut m = app.modes.lock().unwrap();
                            let d = emergency_step(&mut m, trigger, t, hot);
                            if d.is_some() {
                                m.persist();
                            }
                            (d, m.boosted.clone(), m.emergency.as_ref().map(|r| r.release_c).unwrap_or(92))
                        };
                        if let Some(e) = decision {
                            run_emergency(&h2, e, &person, hot, release_c);
                        }
                    },
                )
            });
            // The boost key, where AWCC can't hear it (an admin window in front) or isn't running.
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let mut debounce = boostkey::Debounce::default();
                    let h = handle.clone();
                    let r = boostkey::win::listen(move || {
                        let awcc = { let mut s = sysinfo::System::new(); s.refresh_processes(sysinfo::ProcessesToUpdate::All, true); s.processes().values().any(|p| p.name().to_string_lossy().eq_ignore_ascii_case("AWCC.UCSubAgent.exe")) };
                        if !boostkey::should_handle(boostkey::win::foreground_elevated(), awcc) || !debounce.accept(Instant::now()) {
                            return;
                        }
                        let app = h.state::<App>();
                        let current = app.readings.lock().unwrap().snapshot.as_ref().and_then(|s| s["profile"].as_u64()).map(|p| p as u8);
                        let (default, behaviour) = {
                            let s = app.settings.lock().unwrap();
                            (wiregent_hw::control::parse_profile(&s.default_profile).unwrap_or(0xA0), s.boost_key.clone())
                        };
                        let target = boostkey::key_target(&behaviour, current, app.modes.lock().unwrap().previous, default);
                        let (modes, readings, h2) = (app.modes.clone(), app.readings.clone(), h.clone());
                        std::thread::spawn(move || {
                            let text = match switch_mode(&modes, &readings, target) {
                                Ok(c) => format!("{} (boost key)", c["after"].as_str().unwrap_or("Mode changed")),
                                Err(e) => format!("Boost key: {e}"),
                            };
                            let _ = h2.notification().builder().title("Alien Cockpit").body(&text).show();
                        });
                    });
                    if let Err(e) = r {
                        eprintln!("boost key listener unavailable: {e:#}");
                    }
                });
            }
            // The daily log: the latest reading every `every_s`, cleanup at start and when the day changes.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let dir = log::dir();
                let (mut day, mut last_t): (String, Option<u64>) = (String::new(), None);
                loop {
                    let s = handle.state::<App>().settings.lock().unwrap().log.clone();
                    let today = log::today();
                    if today != day {
                        log::cleanup(&dir, &today, &s);
                        day = today.clone();
                    }
                    if s.enabled {
                        let point = handle.state::<App>().readings.lock().unwrap().history.back().cloned();
                        if let Some(p) = point {
                            if last_t.is_none_or(|t| p.t >= t + s.every_s as u64 * 1_000) {
                                if let Ok(v) = serde_json::to_value(&p) {
                                    let _ = log::append(&dir, &day, &v);
                                }
                                last_t = Some(p.t);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                let line = tray_line(&handle.state::<App>().readings.lock().unwrap());
                if let Some(item) = handle.state::<App>().status_item.lock().unwrap().as_ref() {
                    let _ = item.set_text(&line);
                }
                if let Some(t) = handle.tray_by_id("x17") {
                    let _ = t.set_tooltip(Some(format!("Alien Cockpit OSS\n{line}")));
                }
                std::thread::sleep(Duration::from_secs(2));
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Alien Cockpit OSS")
        // Closing the window destroys it and its web view; the app stays in the tray (Quit is in its menu).
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Quitting: any fan this app raised goes back to the firmware's own curve.
            tauri::RunEvent::Exit => {
                let modes = &app.state::<App>().modes;
                reset_boost(modes);
                forget_raised(modes);
            }
            _ => {}
        });
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

    use wiregent_oem_alienware::rules::Emergency;

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

    /// The window's poll asks only for points it doesn't have yet.
    #[test]
    fn history_is_sent_incrementally() {
        let p = |t: u64| source::Point { t, at: 0, cpu_c: None, gpu_c: None, cpu_w: None, fans: vec![], profile: None, ram_gb: 0.0, cpu_pct: 0.0, gpu_pct: None, net_kbps: None, top: Default::default() };
        let h: std::collections::VecDeque<_> = [1_000, 2_600, 4_200].into_iter().map(p).collect();
        assert_eq!(points_after(&h, None).len(), 3, "the first poll gets everything");
        assert_eq!(points_after(&h, Some(2_600)).iter().map(|x| x.t).collect::<Vec<_>>(), vec![4_200]);
        assert!(points_after(&h, Some(4_200)).is_empty(), "nothing new");
    }

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

    /// While engaged, the person's fan changes are held back before anything reaches the firmware.
    #[test]
    fn fan_changes_wait_while_the_emergency_rule_has_the_fans() {
        let modes = Mutex::new(Modes::default());
        for i in 0..=5 {
            emergency_step(&mut modes.lock().unwrap(), 98, i * 1_000, Some(100));
        }
        assert_eq!(switch_fan_boost(&modes, 0x32, 0), Err(EMERGENCY_HOLD.to_string()));
        assert!(modes.lock().unwrap().boosted.is_empty());
    }

    #[test]
    fn the_hottest_sensor_is_found_in_a_snapshot() {
        assert_eq!(hottest(&json!({ "sensors": [{ "celsius": 63 }, { "celsius": null }, { "celsius": 100 }] })), Some(100));
        assert_eq!(hottest(&json!({ "sensors": [{ "celsius": null }] })), None);
        assert_eq!(hottest(&Value::Null), None);
    }

    #[test]
    fn the_tray_line_says_when_there_is_no_source() {
        assert!(tray_line(&source::State::default()).contains("install the hardware service"));
        let st = source::State { snapshot: Some(json!({ "profile_name": "Balanced", "sensors": [{ "name": "CPU", "celsius": 96 }] })), power: Some(json!({ "cpuPackageW": 24.6 })), source: Some(source::Source::Demo), ..Default::default() };
        assert_eq!(tray_line(&st), "Balanced · CPU 96 °C · 25 W (demo)");
    }
}
