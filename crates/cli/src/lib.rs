//! `cockpit`: every Alien Cockpit OSS function from a terminal, on Windows and Linux.
//!
//! The hardware goes through `wiregent_hw::machine::Machine` (`--backend auto|service|direct|linux|demo`),
//! with the same gates as the app; settings, the log, the emergency rule and the alert texts are
//! cockpit-core's, the same code the app runs. Output is readable text, or one JSON document per line with
//! `--json`.
//!
//! Exit codes: 0 ok, 1 error, 2 refused by a gate or not supported here (usage errors are 2 as well).
//!
//! [`run`] takes the arguments and a writer and returns the exit code, so tests drive every command with
//! `--backend demo` and read what it printed.

use clap::{Parser, Subcommand};
use cockpit_core::{boostkey, log, readout, Settings};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wiregent_hw::control::{self, parse_profile, MAX_BOOST};
use wiregent_hw::machine::Machine;
use wiregent_hw::reader::{FanReading, Snapshot};
use wiregent_oem_alienware::profile_name;
use wiregent_oem_alienware::rules::Emergency;

pub const OK: i32 = 0;
pub const ERROR: i32 = 1;
pub const REFUSED: i32 = 2;

/// The modes by the names the command line takes, in the app's order.
pub const MODE_NAMES: &[(&str, u8)] = &[("quiet", 0xA3), ("balanced", 0xA0), ("performance", 0xA1), ("full-speed", 0xA4), ("battery-saver", 0xA5), ("g-mode", 0xAB)];

const SENSOR_CPU: u8 = 0x01;
const SENSOR_GPU: u8 = 0x06;

#[derive(Parser, Debug)]
#[command(name = "cockpit", version, about = "Alien Cockpit OSS from the command line: readings, thermal modes, fan boost, the emergency fan rule, the daily log and settings.")]
pub struct Cli {
    /// Where readings come from and changes go: auto (the hardware service, else direct when elevated; on
    /// Linux the alienware-wmi driver), service, direct, linux, or demo (the recorded x17 R2 capture).
    #[arg(long, global = true, default_value = "auto", value_parser = ["auto", "service", "direct", "linux", "demo"])]
    pub backend: String,
    /// Machine-readable output: one JSON document per line.
    #[arg(long, global = true)]
    pub json: bool,
    /// The folder with settings.json and logs (default: the app's own, so the app and this share one).
    #[arg(long, global = true, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// The backend, model, BIOS, whether changes are allowed, and the modes and fans the laptop reported.
    Info,
    /// Temperatures, fans (RPM, % of max, zone), the mode and any alerts.
    Status {
        /// Keep reading every SECS seconds until Ctrl+C.
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
        /// With --watch: stop after this many readings.
        #[arg(long, value_name = "N")]
        count: Option<u64>,
    },
    /// The thermal modes this laptop reported, and the current one.
    Modes,
    /// Switch the thermal mode: quiet, balanced, performance, full-speed, battery-saver, g-mode, or an id (0xA0).
    Mode { mode: String },
    /// Fan speeds by zone.
    Fans,
    /// Change fan boost.
    #[command(subcommand)]
    Fan(FanCmd),
    /// Run the emergency fan rule here (as the app does without the hardware service): every fan to full after
    /// 5 s at the trigger, back after 30 s at 6 °C under it. Ctrl+C or the end puts the fans back.
    Guard {
        /// The trigger, °C (85–98; default: the app's setting). It can only be lowered.
        #[arg(long, value_name = "C")]
        trigger: Option<i32>,
        /// Stop after this many readings, then put the fans back (default: until Ctrl+C).
        #[arg(long, value_name = "N")]
        count: Option<u64>,
    },
    /// Battery wear, disk wear, crashes and hardware errors (Windows only).
    Health {
        /// How many days of events to count.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Dell's services against the rules you set (Windows only).
    Drift,
    /// Keyboard and zone lighting, read-only (Windows only).
    #[command(subcommand)]
    Lighting(LightingCmd),
    /// AWCC's game library (Windows only).
    Games,
    /// The daily log.
    #[command(subcommand)]
    Log(LogCmd),
    /// The app's settings (the same file and checks as the app's Settings page).
    #[command(subcommand)]
    Settings(SettingsCmd),
    /// What the boost key (Fn+F1) would do with the current settings.
    #[command(subcommand)]
    BoostKey(BoostKeyCmd),
}

#[derive(Subcommand, Debug)]
pub enum FanCmd {
    /// Raise one fan, the CPU or GPU fans, or all of them: 0 is the firmware's own curve, up to 100.
    Boost {
        /// A fan id (0x32), cpu, gpu or all.
        fan: String,
        /// 0–100.
        level: u32,
    },
    /// Every fan back to the firmware's own curve.
    Reset,
}

#[derive(Subcommand, Debug)]
pub enum LightingCmd {
    /// The AlienFX lighting interfaces (HID), queried read-only.
    Probe,
    /// The lighting AWCC has set, from its database (a read-only copy).
    Show,
}

#[derive(Subcommand, Debug)]
pub enum LogCmd {
    /// Logged days, newest first, with their size.
    Days,
    /// One day's readings (YYYY-MM-DD).
    Read {
        day: String,
        /// At most this many readings, evenly thinned.
        #[arg(long, default_value_t = 1_440)]
        max: usize,
    },
    /// Remove days older than the log setting's limit, then the oldest until the folder fits its size cap.
    Cleanup,
}

#[derive(Subcommand, Debug)]
pub enum SettingsCmd {
    /// Every setting, or one by key ("emergencyC", "log.everyS").
    Get { key: Option<String> },
    /// Change one setting: key=value ("emergencyC=90", "theme=light", "gameModes.warframe=0xA1").
    Set { assignment: String },
}

#[derive(Subcommand, Debug)]
pub enum BoostKeyCmd {
    /// The mode a press would switch to from CURRENT (a mode name, an id, or "none").
    Target {
        current: String,
        /// The mode G-Mode was switched on from (the key goes back to it).
        #[arg(long, value_name = "MODE")]
        previous: Option<String>,
    },
}

/// Time for the commands that repeat (status --watch, guard). Tests pass a clock that doesn't sleep.
pub trait Clock {
    /// A steady clock, ms.
    fn now_ms(&mut self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
    /// From now on, Ctrl+C ends the loop instead of the program.
    fn catch_stop(&mut self) {}
    /// Ctrl+C was pressed.
    fn stopped(&self) -> bool {
        false
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

pub struct RealClock {
    start: Instant,
}

impl Default for RealClock {
    fn default() -> Self {
        RealClock { start: Instant::now() }
    }
}

impl Clock for RealClock {
    fn now_ms(&mut self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn sleep_ms(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while !self.stopped() {
            let now = Instant::now();
            if now >= end {
                break;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(100)));
        }
    }

    fn catch_stop(&mut self) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = ctrlc::set_handler(|| STOP.store(true, Ordering::SeqCst));
        });
    }

    fn stopped(&self) -> bool {
        STOP.load(Ordering::SeqCst)
    }
}

/// Why a command didn't finish.
#[derive(Debug)]
pub enum Fail {
    /// Exit 1.
    Error(String),
    /// Exit 2: a gate refused, or not supported on this system.
    Refused(String),
    /// Already reported; exit with this code.
    Quiet(i32),
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Error(e.to_string())
    }
}

/// The gates' refusals, by their wording in `control` and the hardware service: exit 2, not 1.
const REFUSALS: &[&str] = &["verified", "isn't one this machine reported", "too many changes", "overboost", "isn't cleared", "emergency rule", "not a request this service accepts"];

/// A hardware error, sorted into refused (a gate said no, nothing was written) or failed.
pub fn classify(e: &anyhow::Error) -> Fail {
    let text = format!("{e:#}");
    if REFUSALS.iter().any(|r| text.contains(r)) {
        Fail::Refused(text)
    } else {
        Fail::Error(text)
    }
}

type R = Result<(), Fail>;

/// Runs one command line (`args[0]` is the program name) and returns the exit code.
pub fn run<I, T>(args: I, out: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    run_with(args, out, &mut RealClock::default(), None)
}

/// [`run`] with the clock, and optionally the machine, given (tests).
pub fn run_with<I, T>(args: I, out: &mut dyn Write, clock: &mut dyn Clock, machine: Option<Box<dyn Machine>>) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = write!(out, "{}", e.render());
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => OK,
                _ => REFUSED,
            };
        }
    };
    let json = cli.json;
    let result = {
        let mut ctx = Ctx { json, out: &mut *out, backend: cli.backend, config_dir: cli.config_dir, clock, machine };
        dispatch(&mut ctx, cli.cmd)
    };
    let (code, kind, text) = match result {
        Ok(()) => return OK,
        Err(Fail::Quiet(code)) => return code,
        Err(Fail::Refused(t)) => (REFUSED, "refused", t),
        Err(Fail::Error(t)) => (ERROR, "error", t),
    };
    let _ = if json { writeln!(out, "{}", json!({ "error": text, "refused": code == REFUSED })) } else { writeln!(out, "{kind}: {text}") };
    code
}

struct Ctx<'a> {
    json: bool,
    out: &'a mut dyn Write,
    backend: String,
    config_dir: Option<PathBuf>,
    clock: &'a mut dyn Clock,
    machine: Option<Box<dyn Machine>>,
}

impl Ctx<'_> {
    fn machine(&mut self) -> Result<Box<dyn Machine>, Fail> {
        if let Some(m) = self.machine.take() {
            return Ok(m);
        }
        if let Some(why) = unsupported_backend(&self.backend) {
            return Err(Fail::Refused(why));
        }
        wiregent_hw::machines::select(&self.backend).map_err(|e| Fail::Error(format!("{e:#}")))
    }

    fn settings_path(&self) -> PathBuf {
        self.config_dir.as_ref().map(|d| d.join("settings.json")).unwrap_or_else(Settings::default_path)
    }

    fn log_dir(&self) -> PathBuf {
        self.config_dir.as_ref().map(|d| d.join("logs")).unwrap_or_else(log::dir)
    }

    /// One result: a JSON line with --json, else the text.
    fn emit(&mut self, v: Value, text: &str) -> R {
        if self.json {
            writeln!(self.out, "{v}")?;
        } else {
            writeln!(self.out, "{}", text.trim_end())?;
        }
        Ok(())
    }
}

fn unsupported_backend(backend: &str) -> Option<String> {
    match backend {
        "linux" if !cfg!(target_os = "linux") => Some("the linux backend (the alienware-wmi driver through sysfs) runs on Linux only".into()),
        "service" | "direct" if !cfg!(windows) => Some(format!("the {backend} backend is Windows only; on Linux use --backend linux")),
        _ => None,
    }
}

#[allow(dead_code)]
fn windows_only(what: &str) -> Fail {
    Fail::Refused(format!("`{what}` is Windows only: it reads Windows' own records"))
}

fn dispatch(ctx: &mut Ctx, cmd: Cmd) -> R {
    match cmd {
        Cmd::Info => info(ctx),
        Cmd::Status { watch, count } => status(ctx, watch, count),
        Cmd::Modes => modes(ctx),
        Cmd::Mode { mode } => set_mode(ctx, &mode),
        Cmd::Fans => fans(ctx),
        Cmd::Fan(FanCmd::Boost { fan, level }) => fan_boost(ctx, &fan, level),
        Cmd::Fan(FanCmd::Reset) => fan_reset(ctx),
        Cmd::Guard { trigger, count } => guard(ctx, trigger, count),
        Cmd::Health { days } => health(ctx, days),
        Cmd::Drift => drift(ctx),
        Cmd::Lighting(LightingCmd::Probe) => lighting_probe(ctx),
        Cmd::Lighting(LightingCmd::Show) => lighting_show(ctx),
        Cmd::Games => games(ctx),
        Cmd::Log(LogCmd::Days) => log_days(ctx),
        Cmd::Log(LogCmd::Read { day, max }) => log_read(ctx, &day, max),
        Cmd::Log(LogCmd::Cleanup) => log_cleanup(ctx),
        Cmd::Settings(SettingsCmd::Get { key }) => settings_get(ctx, key.as_deref()),
        Cmd::Settings(SettingsCmd::Set { assignment }) => settings_set(ctx, &assignment),
        Cmd::BoostKey(BoostKeyCmd::Target { current, previous }) => boost_key_target(ctx, &current, previous.as_deref()),
    }
}

// ---------------------------------------------------------------- words and JSON shared by the commands

fn hex(id: u8) -> String {
    format!("0x{id:02X}")
}

/// A mode by its command-line name ("g-mode") or id ("0xAB", "ab", "171").
pub fn parse_mode(s: &str) -> Option<u8> {
    let t = s.trim().to_ascii_lowercase();
    MODE_NAMES.iter().find(|(n, _)| *n == t || n.replace('-', "") == t.replace(['-', '_', ' '], "")).map(|(_, id)| *id).or_else(|| parse_profile(&t))
}

fn mode_arg(id: u8) -> String {
    MODE_NAMES.iter().find(|(_, m)| *m == id).map(|(n, _)| n.to_string()).unwrap_or_else(|| hex(id))
}

fn mode_label(id: u8) -> String {
    format!("{} ({})", profile_name(id), hex(id))
}

fn mode_json(id: u8) -> Value {
    json!({ "id": id, "hex": hex(id), "name": profile_name(id), "arg": mode_arg(id) })
}

fn fan_json(all: &[FanReading], f: &FanReading) -> Value {
    json!({ "id": f.id, "hex": hex(f.id), "zone": readout::zone_name(all, f), "sensor": f.sensor, "rpm": f.rpm, "max_rpm": f.max_rpm, "percent": readout::percent_of_max(f.rpm, f.max_rpm) })
}

fn fan_line(all: &[FanReading], f: &FanReading) -> String {
    let rpm = f.rpm.map_or("—".to_string(), |r| r.to_string());
    let pct = match (readout::percent_of_max(f.rpm, f.max_rpm), f.max_rpm) {
        (Some(p), Some(m)) => format!("  {p:>3}% of {m} RPM max"),
        _ => String::new(),
    };
    format!("{:<12}{}  {:>5} RPM{pct}", readout::zone_name(all, f), hex(f.id), rpm)
}

fn alert_json(a: &wiregent_hw::reader::Alert) -> Value {
    let mut v = serde_json::to_value(a).unwrap_or_default();
    let text = readout::alert_text(&v);
    if let Some(o) = v.as_object_mut() {
        o.insert("text".into(), Value::String(text));
    }
    v
}

fn status_doc(backend: &str, s: &Snapshot) -> (Value, String) {
    let alerts: Vec<Value> = s.alerts.iter().map(alert_json).collect();
    let v = json!({
        "backend": backend,
        "t_ms": s.t_ms,
        "mode": s.profile.map(mode_json),
        "sensors": s.sensors,
        "fans": s.fans.iter().map(|f| fan_json(&s.fans, f)).collect::<Vec<_>>(),
        "alerts": alerts,
    });
    let mut t = String::new();
    if s.sensors.is_empty() && s.fans.is_empty() {
        t.push_str(&format!("No readings from the {backend} backend.\n"));
    }
    t.push_str(&format!("{:<12}{}\n", "Mode", s.profile.map_or("unknown".to_string(), mode_label)));
    for x in &s.sensors {
        let c = x.celsius.map_or("no reading".to_string(), |c| format!("{c} °C"));
        t.push_str(&format!("{:<12}{c}{}\n", x.name, if x.stale { " (stale)" } else { "" }));
    }
    for f in &s.fans {
        t.push_str(&fan_line(&s.fans, f));
        t.push('\n');
    }
    if alerts.is_empty() {
        t.push_str(&format!("{:<12}none\n", "Alerts"));
    }
    for a in &alerts {
        t.push_str(&format!("Alert: {}\n", a["text"].as_str().unwrap_or_default()));
    }
    (v, t)
}

// ---------------------------------------------------------------- hardware commands

fn info(ctx: &mut Ctx) -> R {
    let mut m = ctx.machine()?;
    let (model, bios) = m.identity();
    let verified = control::verified(&model, &bios);
    let s = m.snapshot(ctx.clock.now_ms());
    let profiles = m.profiles();
    let fans = m.fans();
    let zone = |f: u8| s.fans.iter().find(|x| x.id == f).map(|r| readout::zone_name(&s.fans, r));
    let v = json!({
        "backend": m.backend(),
        "model": model,
        "bios": bios,
        "verified": verified,
        "profiles": profiles.iter().map(|p| mode_json(*p)).collect::<Vec<_>>(),
        "fans": fans.iter().map(|f| json!({ "id": f, "hex": hex(*f), "zone": zone(*f) })).collect::<Vec<_>>(),
    });
    let t = format!(
        "{:<10}{}\n{:<10}{}\n{:<10}{}\n{:<10}{}\n{:<10}{}\n{:<10}{}\n",
        "Backend",
        m.backend(),
        "Model",
        if model.is_empty() { "unknown" } else { &model },
        "BIOS",
        if bios.is_empty() { "unknown" } else { &bios },
        "Verified",
        if verified { "yes: modes and fans can be changed" } else { "no: read-only (modes and fans stay as they are)" },
        "Modes",
        profiles.iter().map(|p| mode_label(*p)).collect::<Vec<_>>().join(", "),
        "Fans",
        fans.iter().map(|f| zone(*f).map_or(hex(*f), |z| format!("{z} ({})", hex(*f)))).collect::<Vec<_>>().join(", "),
    );
    ctx.emit(v, &t)
}

fn status(ctx: &mut Ctx, watch: Option<u64>, count: Option<u64>) -> R {
    let mut m = ctx.machine()?;
    if watch.is_some() {
        ctx.clock.catch_stop();
    }
    let mut n = 0u64;
    loop {
        let s = m.snapshot(ctx.clock.now_ms());
        let (v, t) = status_doc(m.backend(), &s);
        if n > 0 && !ctx.json {
            writeln!(ctx.out)?;
        }
        ctx.emit(v, &t)?;
        n += 1;
        let Some(secs) = watch else { break };
        if count.is_some_and(|c| n >= c) || ctx.clock.stopped() {
            break;
        }
        ctx.clock.sleep_ms(secs.saturating_mul(1_000));
        if ctx.clock.stopped() {
            break;
        }
    }
    Ok(())
}

fn modes(ctx: &mut Ctx) -> R {
    let mut m = ctx.machine()?;
    let current = m.snapshot(ctx.clock.now_ms()).profile;
    let profiles = m.profiles();
    let list: Vec<Value> = profiles
        .iter()
        .map(|p| {
            let mut v = mode_json(*p);
            v["current"] = Value::Bool(current == Some(*p));
            v
        })
        .collect();
    let mut t = String::new();
    for p in &profiles {
        t.push_str(&format!("{} {:<14}{}  {}\n", if current == Some(*p) { "*" } else { " " }, profile_name(*p), hex(*p), mode_arg(*p)));
    }
    ctx.emit(json!({ "current": current.map(mode_json), "modes": list }), &t)
}

fn unknown_mode(s: &str) -> Fail {
    Fail::Refused(format!("unknown mode '{s}': use {} or an id like 0xA0", MODE_NAMES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")))
}

fn set_mode(ctx: &mut Ctx, arg: &str) -> R {
    let p = parse_mode(arg).ok_or_else(|| unknown_mode(arg))?;
    let mut m = ctx.machine()?;
    if !m.profiles().contains(&p) {
        return Err(Fail::Refused(format!("{} isn't a mode this laptop reported; it has {}", mode_label(p), m.profiles().iter().map(|x| mode_arg(*x)).collect::<Vec<_>>().join(", "))));
    }
    let c = m.set_profile(p, ctx.clock.now_ms()).map_err(|e| classify(&e))?;
    let none = || "unknown".to_string();
    let t = if c.unchanged {
        format!("Already {}: nothing written.", mode_label(p))
    } else if c.confirmed {
        format!("{}: switched from {}.", mode_label(p), c.before.clone().unwrap_or_else(none))
    } else {
        format!("Asked for {}, but the firmware still reports {}: not confirmed.", mode_label(p), c.after.clone().unwrap_or_else(none))
    };
    let mut v = serde_json::to_value(&c).unwrap_or_default();
    v["id"] = Value::String(hex(p));
    ctx.emit(v, &t)?;
    if c.confirmed {
        Ok(())
    } else {
        Err(Fail::Quiet(ERROR))
    }
}

fn fans(ctx: &mut Ctx) -> R {
    let mut m = ctx.machine()?;
    let s = m.snapshot(ctx.clock.now_ms());
    let t: String = s.fans.iter().map(|f| fan_line(&s.fans, f) + "\n").collect();
    let t = if t.is_empty() { "No fan readings.".to_string() } else { t };
    ctx.emit(json!({ "fans": s.fans.iter().map(|f| fan_json(&s.fans, f)).collect::<Vec<_>>() }), &t)
}

/// The fans `arg` names: an id, "cpu", "gpu" or "all"; only fans the laptop reported.
fn fan_targets(arg: &str, reported: &[u8], s: &Snapshot) -> Result<Vec<u8>, Fail> {
    let by_sensor = |sensor: u8, name: &str| {
        let ids: Vec<u8> = s.fans.iter().filter(|f| f.sensor == Some(sensor) && reported.contains(&f.id)).map(|f| f.id).collect();
        if ids.is_empty() {
            Err(Fail::Refused(format!("no {name} fans reported")))
        } else {
            Ok(ids)
        }
    };
    match arg.trim().to_ascii_lowercase().as_str() {
        "all" => Ok(reported.to_vec()),
        "cpu" => by_sensor(SENSOR_CPU, "CPU"),
        "gpu" => by_sensor(SENSOR_GPU, "GPU"),
        other => match parse_profile(other) {
            Some(f) if reported.contains(&f) => Ok(vec![f]),
            Some(f) => Err(Fail::Refused(format!("fan {} isn't one this laptop reported; it has {}", hex(f), reported.iter().map(|x| hex(*x)).collect::<Vec<_>>().join(", ")))),
            None => Err(Fail::Refused(format!("'{arg}' isn't a fan: use a fan id (0x32), cpu, gpu or all"))),
        },
    }
}

fn fan_boost(ctx: &mut Ctx, arg: &str, level: u32) -> R {
    if level > MAX_BOOST as u32 {
        return Err(Fail::Refused(format!("boost {level} is over {MAX_BOOST}: above that (overboost) needs a per-fan calibration on this laptop first")));
    }
    let mut m = ctx.machine()?;
    let s = m.snapshot(ctx.clock.now_ms());
    let targets = fan_targets(arg, &m.fans(), &s)?;
    let (mut changes, mut text, mut failed, mut unconfirmed) = (Vec::new(), String::new(), None, false);
    for f in targets {
        let zone = s.fans.iter().find(|x| x.id == f).map_or(hex(f), |r| readout::zone_name(&s.fans, r));
        match m.set_fan_boost(f, level as u8, ctx.clock.now_ms()) {
            Ok(c) => {
                let n = |v: Option<u32>| v.map_or("?".to_string(), |x| x.to_string());
                text.push_str(&format!("{zone} ({}): boost {} → {}, {} ({} → {} RPM)\n", hex(f), n(c.before), n(c.after), if c.confirmed { "confirmed" } else { "not confirmed" }, n(c.rpm_before), n(c.rpm_after)));
                unconfirmed |= !c.confirmed;
                let mut v = serde_json::to_value(&c).unwrap_or_default();
                v["zone"] = Value::String(zone);
                changes.push(v);
            }
            Err(e) => {
                failed = Some(classify(&e));
                break;
            }
        }
    }
    if !changes.is_empty() || failed.is_none() {
        ctx.emit(json!({ "changes": changes }), &text)?;
    }
    match failed {
        Some(f) => Err(f),
        None if unconfirmed => Err(Fail::Quiet(ERROR)),
        None => Ok(()),
    }
}

fn fan_reset(ctx: &mut Ctx) -> R {
    let mut m = ctx.machine()?;
    let errors = m.reset_fans();
    if !errors.is_empty() {
        return Err(Fail::Error(format!("some fans didn't go back: {}", errors.join("; "))));
    }
    ctx.emit(json!({ "reset": m.fans().iter().map(|f| hex(*f)).collect::<Vec<_>>() }), "Every fan is back on the firmware's own curve.")
}

fn guard(ctx: &mut Ctx, trigger: Option<i32>, count: Option<u64>) -> R {
    let settings = Settings::load_from(&ctx.settings_path());
    let trigger = trigger.unwrap_or(settings.emergency_c);
    if !(85..=wiregent_oem_alienware::rules::EMERGENCY_MAX_C).contains(&trigger) {
        return Err(Fail::Refused(format!("the trigger must be 85–{} °C: it can only be lowered", wiregent_oem_alienware::rules::EMERGENCY_MAX_C)));
    }
    let mut m = ctx.machine()?;
    if m.backend() == "service" {
        return Err(Fail::Refused(wiregent_hw::machines::SERVICE_RUNS_EMERGENCY.into()));
    }
    let (model, bios) = m.identity();
    let verified = control::verified(&model, &bios);
    let mut modes = cockpit_core::Modes::default();
    let release_c = wiregent_oem_alienware::rules::EmergencyRule::new(trigger).release_c;
    ctx.clock.catch_stop();
    let mut start = format!("Guarding on the {} backend: every fan to full after 5 s at {trigger} °C, back after 30 s at {release_c} °C or cooler. Ctrl+C stops and puts the fans back.", m.backend());
    if !verified {
        start.push_str(" This laptop's model or BIOS isn't verified, so the rule can only warn.");
    }
    ctx.emit(json!({ "event": "start", "backend": m.backend(), "triggerC": trigger, "releaseC": release_c, "verified": verified }), &start)?;
    let looped = (|| -> R {
        let mut n = 0u64;
        while !ctx.clock.stopped() {
            let t = ctx.clock.now_ms();
            let s = m.snapshot(t);
            let hot = cockpit_core::hottest(&serde_json::to_value(&s).unwrap_or_default());
            for a in s.alerts.iter().map(alert_json) {
                let text = format!("Alert: {}", a["text"].as_str().unwrap_or_default());
                ctx.emit(json!({ "event": "alert", "alert": a }), &text)?;
            }
            if let Some(e) = cockpit_core::emergency_step(&mut modes, trigger, t, hot) {
                let errors = m.apply_emergency(e, &modes.boosted);
                let text = readout::emergency_text(e, hot, release_c, &errors);
                let event = if e == Emergency::Engage { "engage" } else { "release" };
                ctx.emit(json!({ "event": event, "t_ms": t, "hottestC": hot, "errors": errors }), &text)?;
            }
            n += 1;
            if count.is_some_and(|k| n >= k) {
                break;
            }
            ctx.clock.sleep_ms(wiregent_oem_alienware::rules::interval_ms(hot, false));
        }
        Ok(())
    })();
    let errors = m.reset_fans();
    let text = if errors.is_empty() { "Stopped: every fan is back on the firmware's own curve.".to_string() } else { format!("Stopped, but some fans didn't go back: {}", errors.join("; ")) };
    ctx.emit(json!({ "event": "stop", "errors": errors }), &text)?;
    looped?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Fail::Quiet(ERROR))
    }
}

// ---------------------------------------------------------------- Windows' own records

#[cfg(windows)]
fn health(ctx: &mut Ctx, days: u32) -> R {
    let h = wiregent_hw::health::read(days);
    let mut t = String::new();
    match &h.battery {
        Some(b) => t.push_str(&format!("Battery     {:.1}% worn ({} of {} mWh{})\n", b.wear_pct, b.full_mwh, b.design_mwh, b.cycles.map_or(String::new(), |c| format!(", {c} cycles")))),
        None => t.push_str("Battery     no reading\n"),
    }
    for d in &h.disks {
        let n = |v: Option<u64>| v.map_or("?".to_string(), |x| x.to_string());
        t.push_str(&format!("Disk        {}: {} used of its rated endurance, {} °C, {} read errors, {} hours on\n", d.name, d.wear_pct.map_or("?".to_string(), |w| format!("{w}%")), n(d.temp_c.map(u64::from)), n(d.read_errors), n(d.power_on_hours)));
    }
    if let Some(e) = &h.events {
        t.push_str(&format!("Last {} days: {} blue screen(s), {} hardware error report(s), {} unexpected shutdown(s)\n", e.days, e.crashes, e.hardware_errors, e.unexpected_shutdowns));
    }
    for f in &h.findings {
        t.push_str(&format!("Finding: {f}\n"));
    }
    for u in &h.unavailable {
        t.push_str(&format!("Unavailable: {u}\n"));
    }
    ctx.emit(serde_json::to_value(&h).unwrap_or_default(), &t)
}

#[cfg(not(windows))]
fn health(_ctx: &mut Ctx, _days: u32) -> R {
    Err(windows_only("health"))
}

#[cfg(windows)]
fn drift(ctx: &mut Ctx) -> R {
    use wiregent_hw::drift::{check, live, Rules};
    let d = check(&Rules::builtin(), &live());
    let ok = d.iter().all(|x| x.severity == "info");
    let mut t = if ok { "Dell's services are as you set them.\n".to_string() } else { String::new() };
    for x in d.iter().filter(|x| x.severity != "info") {
        t.push_str(&format!("[{}] {}: {} now, expected {}{}. {}\n", x.severity, x.service, x.actual, x.expected.join(" or "), if x.running { ", running" } else { "" }, x.why));
    }
    ctx.emit(json!({ "ok": ok, "drift": d }), &t)
}

#[cfg(not(windows))]
fn drift(_ctx: &mut Ctx) -> R {
    Err(windows_only("drift"))
}

#[cfg(windows)]
fn lighting_probe(ctx: &mut Ctx) -> R {
    let r = alienfx::probe::probe().map_err(Fail::Error)?;
    let h = |v: u16| format!("{v:04X}");
    let list: Vec<Value> = r
        .interfaces
        .iter()
        .map(|i| json!({ "path": i.path, "vid": h(i.vid), "pid": h(i.pid), "version": h(i.version), "usage_page": h(i.usage_page), "usage": h(i.usage), "input_len": i.input_len, "output_len": i.output_len, "feature_len": i.feature_len, "api": i.api.map(|a| format!("v{}", a.number())), "report_len": i.report_len, "error": i.error }))
        .collect();
    let mut t = format!("{} HID interfaces seen; AlienFX (187C, 0D62):\n", r.hid_interfaces_seen);
    for i in &r.interfaces {
        t.push_str(&format!("  {}:{} usage {}:{}  {}{}\n", h(i.vid), h(i.pid), h(i.usage_page), h(i.usage), i.api.map_or("not a lighting interface".to_string(), |a| format!("API v{}, {}-byte reports", a.number(), i.report_len.unwrap_or(0))), i.error.as_ref().map_or(String::new(), |e| format!(" ({e})"))));
    }
    if r.interfaces.is_empty() {
        t.push_str("  none found\n");
    }
    ctx.emit(json!({ "hid_interfaces_seen": r.hid_interfaces_seen, "probed_vids": ["187C", "0D62"], "interfaces": list }), &t)
}

#[cfg(not(windows))]
fn lighting_probe(_ctx: &mut Ctx) -> R {
    Err(windows_only("lighting probe"))
}

#[cfg(windows)]
fn lighting_show(ctx: &mut Ctx) -> R {
    use cockpit_core::lighting;
    let devices = lighting::read(&lighting::awcc_db()).map_err(|e| Fail::Error(format!("{e:#}")))?;
    let mut t = String::new();
    for d in &devices {
        t.push_str(&format!("{} [{}]\n", d.device, d.preset));
        for g in &d.groups {
            t.push_str(&format!("  {} LED(s): effect {}, brightness {}, {}\n", g.leds.len(), g.effect, g.brightness, g.colors.join(" ")));
        }
        if !d.power_states.is_empty() {
            t.push_str(&format!("  power states: {}\n", d.power_states.join(", ")));
        }
    }
    if devices.is_empty() {
        t.push_str("AWCC has no active lighting preset.");
    }
    ctx.emit(json!({ "devices": devices }), &t)
}

#[cfg(not(windows))]
fn lighting_show(_ctx: &mut Ctx) -> R {
    Err(windows_only("lighting show"))
}

#[cfg(windows)]
fn games(ctx: &mut Ctx) -> R {
    let g = cockpit_core::games::read();
    let mut t: String = g.iter().map(|x| format!("{} ({}){}\n", x.title, x.provider, x.exe.as_ref().map_or(String::new(), |e| format!("  {e}")))).collect();
    if g.is_empty() {
        t = "No games in AWCC's library.".into();
    }
    ctx.emit(serde_json::to_value(&g).unwrap_or_default(), &t)
}

#[cfg(not(windows))]
fn games(_ctx: &mut Ctx) -> R {
    Err(windows_only("games"))
}

// ---------------------------------------------------------------- the log, settings, the boost key

fn size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.0} KB", (bytes as f64 / 1024.0).ceil())
    }
}

fn log_days(ctx: &mut Ctx) -> R {
    let dir = ctx.log_dir();
    let days = log::days(&dir);
    let total: u64 = days.iter().map(|d| d.bytes).sum();
    let mut t: String = days.iter().map(|d| format!("{}  {}\n", d.day, size(d.bytes))).collect();
    t.push_str(&if days.is_empty() { format!("No logged days in {}.", dir.display()) } else { format!("{} in {}.", size(total), dir.display()) });
    ctx.emit(json!({ "days": days, "totalBytes": total, "dir": dir }), &t)
}

/// "2026-11-09 14:03:07 UTC" from ms since the epoch.
fn utc(ms: u64) -> String {
    let s = ms / 1_000;
    format!("{} {:02}:{:02}:{:02} UTC", log::civil((s / 86_400) as i64), s % 86_400 / 3_600, s % 3_600 / 60, s % 60)
}

fn log_read(ctx: &mut Ctx, day: &str, max: usize) -> R {
    if day.len() != 10 || log::day_number(day).is_none() {
        return Err(Fail::Refused(format!("'{day}' isn't a day: use YYYY-MM-DD")));
    }
    let points = log::read(&ctx.log_dir(), day, max);
    let mut t = String::new();
    for p in &points {
        let c = |k: &str| p[k].as_i64().map_or("—".to_string(), |v| format!("{v} °C"));
        let w = p["cpu_w"].as_f64().map_or(String::new(), |w| format!("  {w:.0} W"));
        let fans: Vec<String> = p["fans"].as_array().into_iter().flatten().map(|f| f.as_u64().map_or("—".into(), |r| r.to_string())).collect();
        let when = p["at"].as_u64().filter(|a| *a > 0).map_or(format!("t={} ms", p["t"]), utc);
        t.push_str(&format!("{when}  {}  CPU {}  GPU {}{w}  fans {} RPM\n", p["profile"].as_str().unwrap_or("—"), c("cpu_c"), c("gpu_c"), fans.join("/")));
    }
    if points.is_empty() {
        t = format!("Nothing logged on {day}.");
    }
    ctx.emit(json!({ "day": day, "points": points }), &t)
}

fn log_cleanup(ctx: &mut Ctx) -> R {
    let s = Settings::load_from(&ctx.settings_path());
    let removed = log::cleanup(&ctx.log_dir(), &log::today(), &s.log);
    let t = if removed.is_empty() { format!("Nothing to remove (keeping {} days, at most {} MB).", s.log.keep_days, s.log.max_mb) } else { format!("Removed {}.", removed.join(", ")) };
    ctx.emit(json!({ "removed": removed }), &t)
}

/// "key = value" lines for every leaf of the settings, dotted like the keys `settings set` takes.
fn flatten(prefix: &str, v: &Value, out: &mut String) {
    match v.as_object() {
        Some(o) if !o.is_empty() => {
            for (k, x) in o {
                flatten(&if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, x, out);
            }
        }
        _ => out.push_str(&format!("{prefix} = {v}\n")),
    }
}

fn settings_get(ctx: &mut Ctx, key: Option<&str>) -> R {
    let s = Settings::load_from(&ctx.settings_path());
    match key {
        None => {
            let v = serde_json::to_value(&s).unwrap_or_default();
            let mut t = String::new();
            flatten("", &v, &mut t);
            ctx.emit(v, &t)
        }
        Some(k) => {
            let v = s.get(k).ok_or_else(|| Fail::Refused(format!("no setting '{k}'")))?;
            let t = format!("{k} = {v}");
            ctx.emit(json!({ "key": k, "value": v }), &t)
        }
    }
}

fn settings_set(ctx: &mut Ctx, assignment: &str) -> R {
    let (k, v) = assignment.split_once('=').map(|(k, v)| (k.trim(), v.trim())).ok_or_else(|| Fail::Refused(format!("'{assignment}' isn't key=value")))?;
    if k == "autostart" {
        return Err(Fail::Refused("starting at sign-in is set in the app's Settings: it creates a Task Scheduler task that starts the app itself".into()));
    }
    let path = ctx.settings_path();
    let s = Settings::load_from(&path).with(k, v).map_err(Fail::Refused)?;
    s.save_to(&path).map_err(Fail::Error)?;
    let value = s.get(k).unwrap_or(Value::Null);
    let t = format!("{k} = {value}\nSaved to {}. A running Alien Cockpit picks this up when it restarts.", path.display());
    ctx.emit(json!({ "key": k, "value": value, "path": path }), &t)
}

fn mode_or_none(s: &str) -> Result<Option<u8>, Fail> {
    if ["none", "unknown", "-"].contains(&s.trim().to_ascii_lowercase().as_str()) {
        return Ok(None);
    }
    parse_mode(s).map(Some).ok_or_else(|| unknown_mode(s))
}

fn boost_key_target(ctx: &mut Ctx, current: &str, previous: Option<&str>) -> R {
    let current = mode_or_none(current)?;
    let previous = previous.map(mode_or_none).transpose()?.flatten();
    let s = Settings::load_from(&ctx.settings_path());
    let default = parse_profile(&s.default_profile).unwrap_or(wiregent_oem_alienware::BALANCED);
    let target = boostkey::key_target(&s.boost_key, current, previous, default);
    let how = if s.boost_key == "cycle" { "steps through Quiet, Balanced, Performance and G-Mode" } else { "toggles G-Mode, back to the mode it left or your default" };
    let t = format!("Boost key ({}: {how}): from {} it switches to {}.", s.boost_key, current.map_or("no reading".to_string(), mode_label), mode_label(target));
    ctx.emit(json!({ "behaviour": s.boost_key, "current": current.map(mode_json), "previous": previous.map(mode_json), "default": mode_json(default), "target": mode_json(target) }), &t)
}
