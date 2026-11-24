//! Alien Cockpit OSS: a standalone app for the Alienware x17 R2 that replaces AWCC and SupportAssist's everyday
//! jobs (docs/plans/x17-standalone-app.md). It talks to the firmware through the hardware service, or
//! directly when it runs as admin. Thermal modes can be switched (gated, confirmed by read-back); fan boost
//! and curves stay locked, with the reason, until they pass their tests. Keyboard lighting is written only
//! in its custom mode (docs/lighting-control.md, the `lights` module).

mod autostart;
mod boostkey;
mod elevate;
mod glow;
mod lights;
mod processes;
mod source;

// Settings, the emergency rule as the app runs it, the daily log and the AWCC readers live in cockpit-core,
// shared with the `cockpit` command line.
use cockpit_core::lightctl::{self, LightAction, LightingSettings, LightingStatus, Mode, PowerEvent};
use cockpit_core::{emergency_step, games, hottest, lighting, log, record_switch, route_for, Modes, Raised, EMERGENCY_HOLD};
pub use cockpit_core::{Effects, Settings, PROFILES};

use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_notification::NotificationExt;

/// Carries out an emergency decision in direct mode and says so (always, whatever the notification
/// setting: this is the one message that must reach the person).
fn run_emergency(app: &AppHandle, e: wiregent_oem_alienware::rules::Emergency, person: &std::collections::BTreeMap<u8, u8>, hot: Option<i32>, release_c: i32) {
    let errors = (|| -> Result<Vec<String>, String> {
        let fw = wiregent_hw::wmi::Awcc::open().map_err(|e| format!("{e:#}"))?;
        let d = wiregent_oem_alienware::Discovery::run(|c| wiregent_hw::Firmware::read(&fw, c).ok()).map_err(|e| e.to_string())?;
        Ok(wiregent_hw::control::apply_emergency(&fw, &d, e, person, wiregent_hw::control::this_machine_verified()))
    })()
    .unwrap_or_else(|e| vec![e]);
    let text = cockpit_core::readout::emergency_text(e, hot, release_c, &errors);
    let _ = app.notification().builder().title("Alien Cockpit OSS: emergency fan rule").body(&text).show();
}

pub struct App {
    readings: source::Shared,
    modes: Arc<Mutex<Modes>>,
    demo: Arc<Mutex<bool>>,
    settings: Mutex<Settings>,
    health: Mutex<Option<(Instant, Value)>>,
    status_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    /// Lighting writes, the AWCC pause lease and the power state, behind one mutex.
    light: lights::Shared,
    /// The keyboard's lighting interface was found (by the last probe).
    keyboard_found: Arc<AtomicBool>,
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
async fn lighting_status(app: State<'_, App>) -> Result<LightingStatus, String> {
    let (s, light, found) = (app.settings.lock().unwrap().lighting.clone(), app.light.clone(), app.keyboard_found.clone());
    tauri::async_runtime::spawn_blocking(move || lights::status(&light, &s, &found)).await.map_err(|e| e.to_string())
}

/// Saves new lighting settings into Settings (validated with the rest), so they replace only `lighting`.
fn save_lighting(app: &App, l: &LightingSettings) -> Result<(), String> {
    l.validate()?;
    let mut all = app.settings.lock().unwrap().clone();
    all.lighting = l.clone();
    all.validate()?;
    all.save()?;
    *app.settings.lock().unwrap() = all;
    Ok(())
}

/// Validates and saves the lighting settings, and writes them when the mode is custom.
#[tauri::command]
async fn lighting_set(app: State<'_, App>, settings: LightingSettings) -> Result<LightingStatus, String> {
    save_lighting(&app, &settings)?;
    let (light, found) = (app.light.clone(), app.keyboard_found.clone());
    tauri::async_runtime::spawn_blocking(move || {
        lights::apply_settings(&light, &settings);
        lights::status(&light, &settings, &found)
    })
    .await
    .map_err(|e| e.to_string())
}

/// One key in one colour on the keyboard, not saved (the map's live preview; custom mode only).
#[tauri::command]
async fn lighting_preview_key(app: State<'_, App>, id: u32, color: String) -> Result<LightingStatus, String> {
    let (s, light, found) = (app.settings.lock().unwrap().lighting.clone(), app.light.clone(), app.keyboard_found.clone());
    tauri::async_runtime::spawn_blocking(move || {
        lights::preview(&light, &s, id, &color)?;
        Ok(lights::status(&light, &s, &found))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Lighting back to AWCC: mode awcc, and AWCC's own preset written to the keyboard.
#[tauri::command]
async fn lighting_restore_awcc(app: State<'_, App>) -> Result<LightingStatus, String> {
    let mut s = app.settings.lock().unwrap().lighting.clone();
    s.mode = Mode::Awcc;
    save_lighting(&app, &s)?;
    let (light, found) = (app.light.clone(), app.keyboard_found.clone());
    tauri::async_runtime::spawn_blocking(move || {
        lights::restore(&light, s.pause_awcc);
        lights::status(&light, &s, &found)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Custom settings seeded from AWCC's look, key by key. The mode is kept and nothing is written.
#[tauri::command]
async fn lighting_import_awcc(app: State<'_, App>) -> Result<LightingStatus, String> {
    let current = app.settings.lock().unwrap().lighting.clone();
    let seeded = tauri::async_runtime::spawn_blocking(move || lights::import(&current)).await.map_err(|e| e.to_string())??;
    save_lighting(&app, &seeded)?;
    let (light, found) = (app.light.clone(), app.keyboard_found.clone());
    tauri::async_runtime::spawn_blocking(move || lights::status(&light, &seeded, &found)).await.map_err(|e| e.to_string())
}

/// A power change: darkens or re-applies the keyboard by the settings' rules (custom mode only). Dark is
/// written before this returns (before the laptop sleeps); a re-apply waits `after_ms` on its own thread.
fn lighting_power_event(handle: &AppHandle, ev: PowerEvent) {
    let app = handle.state::<App>();
    let s = app.settings.lock().unwrap().lighting.clone();
    let action = lightctl::on_power_event(&mut lights::lock(&app.light).power, ev, &s);
    match action {
        LightAction::Nothing => {}
        LightAction::Dark => {
            lights::apply_settings(&app.light, &s);
        }
        LightAction::Reapply { after_ms } => {
            let h = handle.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(after_ms));
                let app = h.state::<App>();
                let s = app.settings.lock().unwrap().lighting.clone();
                lights::apply_settings(&app.light, &s);
            });
        }
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
async fn settings_set(app: State<'_, App>, mut settings: Settings) -> Result<Settings, String> {
    // Lighting is owned by the Lighting page (`lighting_set`). The Settings page sends the whole file from the
    // copy it loaded, so its `lighting` block may be stale: keep the current one.
    settings.lighting = app.settings.lock().unwrap().lighting.clone();
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

/// At start: fans a previous run raised and never put back (it crashed or was killed) go back to the
/// firmware's curve. The emergency rule re-engages within 5 s if it's still needed. Returns what was done.
fn recover_raised(modes: &Mutex<Modes>) -> Option<String> {
    let left = Raised::load_from(&Raised::default_path())?;
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
        let _ = Raised::default().save_to(&Raised::default_path());
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
        let _ = Raised::default().save_to(&Raised::default_path());
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
        "lighting": lighting_control(&app),
    }))
}

/// The lighting control's state for `controls`: enabled when the keyboard's lighting interface was found.
fn lighting_control(app: &App) -> Value {
    let found = app.keyboard_found.load(std::sync::atomic::Ordering::SeqCst);
    let mode = app.settings.lock().unwrap().lighting.mode;
    let reasons: Vec<&str> = if found { vec![] } else { vec!["The keyboard's lighting interface wasn't found."] };
    json!({
        "enabled": found,
        "mode": mode,
        "reasons": reasons,
        "notes": ["Zone colours are sent but don't light on this laptop.", "In AWCC mode nothing is written: AWCC's look stays."],
    })
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
    let (glow_keys, glow_rx) = glow::Keys::new();
    let glow_keys = Arc::new(glow_keys);
    let mut glow_rx = Some(glow_rx);
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .manage(App { readings: readings.clone(), modes: Arc::new(Mutex::new(Modes::default())), demo: demo.clone(), settings: Mutex::new(settings), health: Mutex::new(None), status_item: Mutex::new(None), light: Default::default(), keyboard_found: Default::default() })
        .invoke_handler(tauri::generate_handler![status, history, health, drift, lighting, lighting_status, lighting_set, lighting_preview_key, lighting_restore_awcc, lighting_import_awcc, game_library, settings_get, settings_set, controls, set_mode, set_fan_boost, log_days, log_read, log_cleanup, open_alienware_update, restart_elevated])
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
                let keys = glow_keys.clone();
                std::thread::spawn(move || {
                    let mut debounce = boostkey::Debounce::default();
                    let h = handle.clone();
                    let r = boostkey::win::listen(
                        move || {
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
                        },
                        // Every key-down, for the reactive glow (dropped at once while it isn't running).
                        move |make, flags| keys.key_down(make, flags),
                    );
                    if let Err(e) = r {
                        eprintln!("boost key listener unavailable: {e:#}");
                    }
                });
                // The reactive glow: its own thread, frames under the lighting lock.
                if let Some(rx) = glow_rx.take() {
                    let handle = app.handle().clone();
                    let (light, keys) = (app.state::<App>().light.clone(), glow_keys.clone());
                    std::thread::spawn(move || glow::run(light, keys, rx, move || handle.state::<App>().settings.lock().unwrap().lighting.clone()));
                }
            }
            // Lighting: find the keyboard (read-only probe), and in custom mode put the person's look back on
            // it (AWCC sets its own at sign-in).
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                // Before anything else: AWCC's lighting programs resumed once, in case this app crashed while
                // its 2 s pause lease held them (see `Pauser::recover`).
                lights::recover();
                let app = handle.state::<App>();
                let s = app.settings.lock().unwrap().lighting.clone();
                let _ = lights::status(&app.light, &s, &app.keyboard_found);
                if s.mode == Mode::Custom {
                    std::thread::sleep(Duration::from_millis(lightctl::RESUME_DELAY_MS));
                    let s = app.settings.lock().unwrap().lighting.clone();
                    lights::apply_settings(&app.light, &s);
                }
            });
            // Power changes darken and restore the keyboard (custom mode only).
            #[cfg(windows)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let h = handle.clone();
                    if let Err(e) = lights::win::listen(move |ev| lighting_power_event(&h, ev)) {
                        eprintln!("power notifications unavailable: {e:#}");
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
                // The reactive glow stops with the static colours written back; AWCC's lighting programs resume
                // now if a lighting write still holds them.
                lights::quit(&app.state::<App>().light);
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's poll asks only for points it doesn't have yet.
    #[test]
    fn history_is_sent_incrementally() {
        let p = |t: u64| source::Point { t, at: 0, cpu_c: None, gpu_c: None, cpu_w: None, fans: vec![], profile: None, ram_gb: 0.0, cpu_pct: 0.0, gpu_pct: None, net_kbps: None, top: Default::default() };
        let h: std::collections::VecDeque<_> = [1_000, 2_600, 4_200].into_iter().map(p).collect();
        assert_eq!(points_after(&h, None).len(), 3, "the first poll gets everything");
        assert_eq!(points_after(&h, Some(2_600)).iter().map(|x| x.t).collect::<Vec<_>>(), vec![4_200]);
        assert!(points_after(&h, Some(4_200)).is_empty(), "nothing new");
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
    fn the_tray_line_says_when_there_is_no_source() {
        assert!(tray_line(&source::State::default()).contains("install the hardware service"));
        let st = source::State { snapshot: Some(json!({ "profile_name": "Balanced", "sensors": [{ "name": "CPU", "celsius": 96 }] })), power: Some(json!({ "cpuPackageW": 24.6 })), source: Some(source::Source::Demo), ..Default::default() };
        assert_eq!(tray_line(&st), "Balanced · CPU 96 °C · 25 W (demo)");
    }
}
