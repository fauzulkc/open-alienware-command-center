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
//! `--backend demo` and read what it printed. [`run_with_lights`] also takes the lighting devices, so tests
//! hand `cockpit lighting` a fake keyboard (`cockpit_core::lightctl::FakeIo`) and never reach the real one.

use clap::{Parser, Subcommand};
use cockpit_core::lightctl::{self, KeyEffect, LightIo, LightingSettings, Mode, PowerState, Shown};
use cockpit_core::{boostkey, log, mousepaint, reactive, readout, screenmirror, Settings};
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
    /// CPU and GPU clocks now, like CPU-Z and GPU-Z (Windows only): each core's running clock (P- and
    /// E-cores), and the NVIDIA GPU's core and memory clocks, power state, power, load and temperature.
    Clocks {
        /// Leave the NVIDIA GPU out (reading it can wake it from sleep).
        #[arg(long)]
        no_gpu: bool,
        /// Read this many times, a second apart (default once).
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Battery wear, disk wear, crashes and hardware errors (Windows only).
    Health {
        /// How many days of events to count.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Dell's services against the rules you set (Windows only).
    Drift,
    /// Keyboard lighting: AWCC's look, and the keyboard's colours, brightness and effects (writes are Windows
    /// only). Changes save the app's own lighting settings; they are written when the mode is custom.
    Lighting {
        /// Print the reports a command would send, and change nothing (no save, no write).
        #[arg(long, global = true)]
        dry_run: bool,
        #[command(subcommand)]
        cmd: LightingCmd,
    },
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
    /// The lighting settings, the key layout, the devices found and AWCC's look.
    Status,
    /// Every key's base colour (#RRGGBB).
    Color { color: String },
    /// One of AWCC's key groups (Function Keys, Numbers, QWER, WASD) in a colour, over the base.
    Group { name: String, color: String },
    /// One key, by id (0-135) or name ("Esc", "Left Shift"), in a colour, over its group.
    Key { key: String, color: String },
    /// Backlight brightness, 0-100 %.
    Brightness { percent: u32 },
    /// Backlight off (colours kept).
    Off,
    /// Backlight on.
    On,
    /// static (per-key colours) or a keyboard effect: breathing, wave, dual-wave, pulse, mix-pulse,
    /// night-rider, laser.
    Effect {
        effect: String,
        /// 0-255, passed through as alien-tools sends it.
        #[arg(long)]
        tempo: Option<u32>,
        /// The second colour for dual-wave and mix-pulse.
        #[arg(long, value_name = "#RRGGBB")]
        color2: Option<String>,
    },
    /// Write AWCC's own look back to the keyboard and hand lighting back to AWCC (mode awcc).
    RestoreAwcc,
    /// Seed the custom settings from AWCC's look, key by key (nothing is written).
    ImportAwcc,
    /// awcc: AWCC's look stays and nothing is written; custom: these settings are written now and kept.
    Mode {
        #[arg(value_parser = ["awcc", "custom"])]
        mode: String,
    },
    /// The reactive glow in the foreground (Windows): keys warm up as you press them, with the saved custom
    /// colours and glow settings. Ctrl+C or --seconds stops it and writes the static colours back. Key
    /// presses are read with Raw Input, read-only, and never stored. --dry-run simulates typing instead and
    /// prints how many reports each second would send.
    Reactive {
        /// Stop after this many seconds (default: until Ctrl+C; a dry run simulates 10).
        #[arg(long, value_name = "N")]
        seconds: Option<u64>,
    },
    /// The local time across the whole keyboard, in the foreground: h:mm in big digits (hour peach, minutes
    /// cyan) with a steady pink colon, and for 5 s of every 15 the seconds in big pink digits.
    /// Ctrl+C or --seconds stops it and writes the static colours back.
    Clock {
        /// Stop after this many seconds (default: until Ctrl+C; a dry run simulates 10).
        #[arg(long, value_name = "N")]
        seconds: Option<u64>,
        /// The other keys: off (dark), dim (the static colours dimmed), or preset (as they are).
        #[arg(long, default_value = "off", value_parser = ["off", "dim", "preset"])]
        base: String,
    },
    /// Mouse paint, in the foreground (Windows): the screen mapped onto the keyboard; the key under the mouse
    /// pointer lights up and fades back (--fade-ms), so the mouse leaves a trail. Looks: glow (your colours
    /// stay, the mouse lights keys white), uncover (white keyboard, the mouse shows each key's own colour),
    /// reveal (dark keyboard, the mouse brings each key's own colour through), boost (your colours
    /// dimmed to --dim %, the mouse brings keys to full), screen (dark keyboard, the screen's colour under the pointer).
    /// The pointer's position is polled (no input hook). Ctrl+C or --seconds stops it and writes the static
    /// colours back.
    Mouse {
        /// Stop after this many seconds (default: until Ctrl+C; a dry run simulates 5).
        #[arg(long, value_name = "N")]
        seconds: Option<u64>,
        /// Writes a second at most, 1-30.
        #[arg(long, default_value_t = 10)]
        fps: u32,
        /// How long a painted key takes to fade back, ms (0 keeps the paint), 0-10000.
        #[arg(long, default_value_t = cockpit_core::mousepaint::DEFAULT_FADE_MS)]
        fade_ms: u64,
        /// For look boost: the dimmed keys' brightness, % of your colours, 1-100.
        #[arg(long, default_value_t = cockpit_core::mousepaint::DEFAULT_DIM_PCT)]
        dim: u32,
        /// glow, uncover, reveal, boost or screen.
        #[arg(long, default_value = "glow", value_parser = ["glow", "uncover", "reveal", "boost", "screen"])]
        look: String,
    },
    /// Live lighting, in the foreground (Windows), as the app runs it: your static colours, an effect (none,
    /// clock, screen mirror) and live interactions on top (key presses, the mouse). Starts from the saved
    /// `lighting.live`; the options here replace parts of it for this run only (nothing is saved). Ctrl+C or
    /// --seconds stops it and writes the static colours back.
    Live {
        /// Stop after this many seconds (default: until Ctrl+C).
        #[arg(long, value_name = "N")]
        seconds: Option<u64>,
        /// The effect over your colours: none, clock or screen (the mirror).
        #[arg(long, value_parser = ["none", "clock", "screen"])]
        effect: Option<String>,
        /// On key presses, comma-separated: laser (a beam both ways along the pressed key's row), lightning
        /// (bolts in from both ends of the row, striking the key), seismic (rings over the whole keyboard in
        /// the key's own colour); or none. Key presses are read read-only and never stored.
        #[arg(long, value_name = "laser,lightning,seismic")]
        tap: Option<String>,
        /// The laser's and lightning's colour.
        #[arg(long, value_name = "#RRGGBB")]
        tap_color: Option<String>,
        /// How fast the laser and lightning run, keys a second (5-80).
        #[arg(long)]
        tap_speed: Option<u32>,
        /// The mouse: off, glow (a white trail), screen (a screen-colour trail), or uncover, reveal, boost (the
        /// static colours where the pointer goes over a white, dark or dimmed keyboard).
        #[arg(long, value_parser = ["off", "glow", "screen", "uncover", "reveal", "boost"])]
        mouse: Option<String>,
        /// Run as if the display were off (with the clock for it).
        #[arg(long)]
        display_off: bool,
    },
    /// Screen mirror, in the foreground (Windows): the keyboard as an ambient light for the screen. The screen
    /// maps onto the keys as in mouse paint (top-left = Esc, bottom-right = Right Arrow); the whole keyboard is
    /// one gradient of the screen's areas, pulled toward its average (--spread), deepened (--saturation) and
    /// eased (--smooth-ms), all keys updated together. Ctrl+C or --seconds stops it and writes the static
    /// colours back.
    Screen {
        /// Stop after this many seconds (default: until Ctrl+C; a dry run simulates 5).
        #[arg(long, value_name = "N")]
        seconds: Option<u64>,
        /// Writes a second at most, 1-30.
        #[arg(long, default_value_t = cockpit_core::screenmirror::DEFAULT_FPS)]
        fps: u32,
        /// How slowly the keys follow the screen: the ease's time constant, ms, 0-10000.
        #[arg(long, default_value_t = cockpit_core::screenmirror::DEFAULT_SMOOTH_MS)]
        smooth_ms: u64,
        /// 0-1: how much the screen's areas keep their own colours (0 = the whole keyboard one average colour).
        #[arg(long, default_value_t = cockpit_core::screenmirror::DEFAULT_SPREAD)]
        spread: f64,
        /// 1-4: how much deeper the colours are than the screen's averages (1 = as averaged).
        #[arg(long, default_value_t = cockpit_core::screenmirror::DEFAULT_SATURATION)]
        saturation: f64,
        /// gpu: read the screen on the graphics card (least processor, more memory); cpu: through GDI.
        #[arg(long, default_value = "gpu", value_parser = ["gpu", "cpu"])]
        reader: String,
    },
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
    run_with_lights(args, out, clock, machine, None)
}

/// [`run_with`] with the lighting devices given too (tests pass `FakeIo`; `None` is the real ones).
pub fn run_with_lights<I, T>(args: I, out: &mut dyn Write, clock: &mut dyn Clock, machine: Option<Box<dyn Machine>>, lights: Option<Box<dyn LightIo>>) -> i32
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
        let mut ctx = Ctx { json, out: &mut *out, backend: cli.backend, config_dir: cli.config_dir, clock, machine, lights };
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
    lights: Option<Box<dyn LightIo>>,
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
        Cmd::Clocks { no_gpu, count } => clocks(ctx, !no_gpu, count),
        Cmd::Drift => drift(ctx),
        Cmd::Lighting { cmd: LightingCmd::Probe, .. } => lighting_probe(ctx),
        Cmd::Lighting { cmd: LightingCmd::Show, .. } => lighting_show(ctx),
        Cmd::Lighting { cmd, dry_run } => lighting_cmd(ctx, cmd, dry_run),
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

/// The clocks as text: CPU average and fastest core, each core, then the GPU.
pub fn clocks_text(c: &wiregent_hw::clocks::Clocks) -> String {
    let ghz = |m: f64| format!("{:.2} GHz", m / 1_000.0);
    let mut t = String::new();
    match &c.cpu {
        Some(cpu) => {
            t.push_str(&format!("CPU         {} average, fastest core {}\n", ghz(cpu.avg_mhz), ghz(cpu.max_mhz)));
            for k in &cpu.cores {
                t.push_str(&format!("  {:<4} {:>9}  (rated {:.1} GHz, threads {:?})\n", k.label, ghz(k.mhz), k.base_mhz as f64 / 1_000.0, k.threads));
            }
        }
        None => t.push_str(&format!("CPU         {}\n", c.cpu_error.as_deref().unwrap_or("no reading"))),
    }
    match &c.gpu {
        Some(g) => {
            let mhz = |v: Option<u32>| v.map_or("-".to_string(), |v| format!("{v} MHz"));
            t.push_str(&format!("GPU         {}\n", g.name));
            t.push_str(&format!("  core     {} of {}\n", mhz(g.core_mhz), mhz(g.max_core_mhz)));
            t.push_str(&format!("  memory   {} of {}\n", mhz(g.mem_mhz), mhz(g.max_mem_mhz)));
            t.push_str(&format!("  state    {}, {}, load {}, {}\n", g.pstate.map_or("-".into(), |p| format!("P{p}")), g.power_w.map_or("-".into(), |w| format!("{w:.1} W")), g.load_pct.map_or("-".into(), |l| format!("{l} %")), g.temp_c.map_or("-".into(), |c| format!("{c} °C"))));
        }
        None => {
            if let Some(e) = &c.gpu_error {
                t.push_str(&format!("GPU         {e}\n"));
            }
        }
    }
    if let Some(p) = &c.power {
        let w = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.1} W"));
        t.push_str(&format!("Power       measured {:.1} W\n", p.measured_w));
        t.push_str(&format!("  CPU      {} (cores {}, graphics {}, rest of the chip {})\n", w(p.cpu_package_w), w(p.cpu_cores_w), w(p.igpu_w), w(p.cpu_rest_w)));
        if p.dram_w.is_some() {
            t.push_str(&format!("  memory   {}\n", w(p.dram_w)));
        }
        t.push_str(&format!("  GPU      {}{}\n", w(p.gpu_w), p.gpu_limit_w.map_or(String::new(), |l| format!(" of {l:.0} W"))));
        match (&p.battery, p.input_w) {
            (Some(b), Some(i)) if !b.on_ac => t.push_str(&format!("  input    {i:.1} W from the battery; not measured {}\n", w(p.rest_w))),
            (Some(b), _) => t.push_str(&format!("  input    on AC (Windows doesn't report the adapter); charging the battery {:.1} W\n", b.charging_w)),
            (None, _) => t.push_str("  input    no battery reading\n"),
        }
    }
    t
}

#[cfg(windows)]
fn clocks(ctx: &mut Ctx, gpu: bool, count: u32) -> R {
    let mut r = wiregent_hw::clocks::win::ClockReader::new();
    // The CPU clocks are averages since the last read: give the first one a second.
    ctx.clock.sleep_ms(1_000);
    let mut last = r.read(gpu);
    for i in 1..count.max(1) {
        if !ctx.json {
            writeln!(ctx.out, "{}", clocks_text(&last))?;
        }
        let _ = i;
        ctx.clock.sleep_ms(1_000);
        last = r.read(gpu);
    }
    ctx.emit(serde_json::to_value(&last).unwrap_or_default(), clocks_text(&last).trim_end())
}

#[cfg(not(windows))]
fn clocks(_ctx: &mut Ctx, _gpu: bool, _count: u32) -> R {
    Err(windows_only("clocks"))
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

// ---------------------------------------------------------------- lighting control

impl Ctx<'_> {
    /// The lighting devices: the test's, or the real ones.
    fn light_io(&mut self) -> Box<dyn LightIo> {
        self.lights.take().unwrap_or_else(|| Box::new(lightctl::SystemIo))
    }

    /// Writes (and reading AWCC's look) need Windows, unless a test gave the devices.
    fn lights_here(&self) -> R {
        if self.lights.is_none() && !cfg!(windows) {
            return Err(Fail::Refused(format!("{}: the keyboard is written through Windows' HID driver", lightctl::WINDOWS_ONLY)));
        }
        Ok(())
    }
}

fn lighting_cmd(ctx: &mut Ctx, cmd: LightingCmd, dry_run: bool) -> R {
    match cmd {
        LightingCmd::Probe | LightingCmd::Show => unreachable!("dispatched above"),
        LightingCmd::Status => lighting_status(ctx),
        LightingCmd::Color { color } => lighting_change(ctx, dry_run, move |l| {
            l.keyboard.color = color;
            Ok(())
        }),
        LightingCmd::Group { name, color } => {
            let g = alienfx::x17::group(&name).ok_or_else(|| Fail::Refused(format!("'{name}' isn't a key group: use {}", lightctl::group_names().join(", "))))?;
            lighting_change(ctx, dry_run, move |l| {
                l.keyboard.groups.insert(g.name.to_string(), color);
                Ok(())
            })
        }
        LightingCmd::Key { key, color } => {
            let id = key.trim().parse::<u32>().ok().or_else(|| alienfx::x17::by_name(&key).map(|k| k.id as u32)).ok_or_else(|| Fail::Refused(format!("'{key}' isn't a key: use an id (0-135) or a name like Esc or Left Shift")))?;
            lighting_change(ctx, dry_run, move |l| {
                l.keyboard.keys.insert(id, color);
                Ok(())
            })
        }
        LightingCmd::Brightness { percent } => lighting_change(ctx, dry_run, move |l| {
            l.keyboard.brightness = percent;
            Ok(())
        }),
        LightingCmd::Off => lighting_change(ctx, dry_run, |l| {
            l.keyboard.on = false;
            Ok(())
        }),
        LightingCmd::On => lighting_change(ctx, dry_run, |l| {
            l.keyboard.on = true;
            Ok(())
        }),
        LightingCmd::Effect { effect, tempo, color2 } => {
            let e = KeyEffect::parse(&effect).ok_or_else(|| Fail::Refused(format!("'{effect}' isn't an effect: use {}", KeyEffect::ALL.iter().map(|e| e.name()).collect::<Vec<_>>().join(", "))))?;
            lighting_change(ctx, dry_run, move |l| {
                l.keyboard.effect = e;
                if let Some(t) = tempo {
                    l.keyboard.tempo = t;
                }
                if let Some(c) = color2 {
                    l.keyboard.color2 = c;
                }
                Ok(())
            })
        }
        LightingCmd::Mode { mode } => lighting_change(ctx, dry_run, move |l| {
            l.mode = if mode == "custom" { Mode::Custom } else { Mode::Awcc };
            Ok(())
        }),
        LightingCmd::RestoreAwcc => lighting_restore(ctx, dry_run),
        LightingCmd::ImportAwcc => lighting_import(ctx, dry_run),
        LightingCmd::Reactive { seconds } => lighting_reactive(ctx, seconds, dry_run),
        LightingCmd::Clock { seconds, base } => lighting_clock(ctx, seconds, &base, dry_run),
        LightingCmd::Live { seconds, effect, tap, tap_color, tap_speed, mouse, display_off } => lighting_live(ctx, seconds, LiveOverrides { effect, tap, tap_color, tap_speed, mouse, display_off }),
        LightingCmd::Screen { seconds, fps, smooth_ms, spread, saturation, reader } => lighting_screen(ctx, seconds, screenmirror::Ambient { fps, smooth_ms, spread, saturation }, reader == "gpu", dry_run),
        LightingCmd::Mouse { seconds, fps, fade_ms, dim, look } => lighting_mouse(ctx, seconds, fps, fade_ms, dim, &look, dry_run),
    }
}

fn lighting_summary(l: &LightingSettings) -> String {
    let k = &l.keyboard;
    let yes = |b: bool| if b { "yes" } else { "no" };
    let mode = match l.mode {
        Mode::Awcc => "awcc: AWCC's look stays (nothing is written)",
        Mode::Custom => "custom: these settings are written to the keyboard",
    };
    let effect = if k.effect == KeyEffect::Static { "static".to_string() } else { format!("{} (tempo {}, colours {} {})", k.effect.name(), k.tempo, k.color, k.color2) };
    format!(
        "{:<12}{mode}\n{:<12}{}, {} %, {effect}, base {}, {} group colour(s), {} key colour(s)\n{:<12}dark on sleep: {}, lid closed: {}, display off: {}; 30 % on Energy Saver: {}; pause AWCC: {}\n",
        "Mode",
        "Keyboard",
        if k.on { "on" } else { "off" },
        k.brightness,
        k.color,
        k.groups.len(),
        k.keys.len(),
        "Power",
        yes(l.off_on_sleep),
        yes(l.off_on_lid_close),
        yes(l.off_on_display_off),
        yes(l.dim_on_battery_saver),
        yes(l.pause_awcc),
    )
}

fn lighting_status(ctx: &mut Ctx) -> R {
    let l = Settings::load_from(&ctx.settings_path()).lighting;
    let mut io = ctx.light_io();
    let awcc = io.awcc().ok().flatten();
    let device = io.detect();
    let st = lightctl::status(&l, awcc, device, None);
    let mut t = lighting_summary(&l);
    let d = &st.device;
    t.push_str(&format!(
        "{:<12}keyboard {}; zone controller {}{}\n",
        "Devices",
        if d.keyboard { "found" } else { "not found" },
        if d.zones { "found (its lights don't show on this laptop)" } else { "not found" },
        d.error.as_ref().map_or(String::new(), |e| format!(" ({e})"))
    ));
    match &st.awcc {
        Some(a) => t.push_str(&format!("{:<12}\"{}\", {} key(s)\n", "AWCC", a.preset, a.keys.len())),
        None => t.push_str(&format!("{:<12}no keyboard preset read\n", "AWCC")),
    }
    ctx.emit(serde_json::to_value(&st).unwrap_or_default(), &t)
}

/// A dry run's output: what would be sent, nothing saved or written.
fn dry_run_doc(l: &LightingSettings, kb: &[String], zones: &[String], note: &str) -> (Value, String) {
    let mut t = format!("Dry run: nothing saved, nothing sent. {note}\n");
    if !kb.is_empty() {
        t.push_str(&format!("Keyboard ({} step(s)):\n", kb.len()));
        kb.iter().for_each(|r| t.push_str(&format!("  {r}\n")));
    }
    if !zones.is_empty() {
        t.push_str(&format!("Zone controller ({} step(s), not visible on this laptop):\n", zones.len()));
        zones.iter().for_each(|r| t.push_str(&format!("  {r}\n")));
    }
    (json!({ "dryRun": true, "settings": l, "keyboard": kb, "zones": zones, "note": note }), t)
}

/// One change to the lighting settings: validated, saved to the app's file, and written when the mode is
/// custom (with AWCC paused when `pauseAwcc`). `--dry-run` prints the reports instead.
fn lighting_change(ctx: &mut Ctx, dry_run: bool, f: impl FnOnce(&mut LightingSettings) -> R) -> R {
    let path = ctx.settings_path();
    let mut all = Settings::load_from(&path);
    // What the keyboard shows now, as far as the saved settings say (AWCC's own state in awcc mode): only
    // what changes from it is sent.
    let prev = shown_by(&all.lighting);
    let mut l = all.lighting.clone();
    f(&mut l)?;
    l.validate().map_err(Fail::Refused)?;
    all.lighting = l.clone();
    all.validate().map_err(Fail::Refused)?;
    let custom = l.mode == Mode::Custom;
    let power = PowerState::default();
    if dry_run {
        let (kb, zones) = if custom { (lightctl::plan_from(prev, &l, &power).map_err(Fail::Refused)?, lightctl::zone_plan(&l, &power).map_err(Fail::Refused)?) } else { (Vec::new(), Vec::new()) };
        let note = if custom { "Mode custom: a real run saves and sends these." } else { "Mode awcc: a real run saves, and sends nothing." };
        let (v, t) = dry_run_doc(&l, &lightctl::describe(&kb), &lightctl::describe(&zones), note);
        return ctx.emit(v, &t);
    }
    ctx.lights_here()?;
    all.save_to(&path).map_err(Fail::Error)?;
    if !custom {
        let t = format!("{}Saved to {}. Not written: the mode is awcc, so AWCC's look stays (`cockpit lighting mode custom` writes these).", lighting_summary(&l), path.display());
        return ctx.emit(json!({ "settings": l, "saved": path, "applied": false }), &t);
    }
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    // Before anything else: AWCC resumed once, in case a crash left it paused (see `Pauser::recover`).
    pauser.recover();
    match lightctl::write_from(io.as_mut(), prev, &l, &power, l.pause_awcc.then_some(pauser.as_ref())) {
        Ok(a) => {
            let t = format!("{}Saved to {}. Written: {}", lighting_summary(&l), path.display(), a.text());
            ctx.emit(json!({ "settings": l, "saved": path, "applied": true, "result": a }), &t)
        }
        Err(e) => Err(Fail::Error(format!("saved to {}, but writing the keyboard failed: {e}", path.display()))),
    }
}

/// What the keyboard shows under these saved settings: theirs in custom mode, AWCC's own state otherwise.
fn shown_by(l: &LightingSettings) -> Shown {
    if l.mode == Mode::Custom {
        Shown::of(l, &PowerState::default())
    } else {
        Shown::AWCC
    }
}

fn awcc_look(io: &mut dyn LightIo) -> Result<lightctl::AwccLook, Fail> {
    io.awcc().map_err(Fail::Error)?.ok_or_else(|| Fail::Error("AWCC has no active keyboard preset".into()))
}

fn lighting_restore(ctx: &mut Ctx, dry_run: bool) -> R {
    ctx.lights_here()?;
    let path = ctx.settings_path();
    let mut all = Settings::load_from(&path);
    let mut io = ctx.light_io();
    if !dry_run {
        // Before anything else: AWCC resumed once, in case a crash left it paused (see `Pauser::recover`).
        io.pauser().recover();
    }
    let look = awcc_look(io.as_mut())?;
    let prev = shown_by(&all.lighting);
    let mut l = all.lighting.clone();
    l.mode = Mode::Awcc;
    if dry_run {
        let steps = lightctl::restore_awcc_plan_from(prev, &look).map_err(Fail::Refused)?;
        let (v, t) = dry_run_doc(&l, &lightctl::describe(&steps), &[], &format!("A real run writes AWCC's \"{}\" and sets the mode to awcc.", look.preset));
        return ctx.emit(v, &t);
    }
    all.lighting = l.clone();
    all.save_to(&path).map_err(Fail::Error)?;
    let pauser = io.pauser();
    let (look, a) = lightctl::restore_from(io.as_mut(), prev, l.pause_awcc.then_some(pauser.as_ref())).map_err(|e| Fail::Error(format!("the mode is awcc again, but writing AWCC's look failed: {e}")))?;
    let t = format!("AWCC's \"{}\" is back on the keyboard ({} keys) and lighting is AWCC's again (mode awcc). {}", look.preset, look.keys.len(), a.text());
    ctx.emit(json!({ "settings": l, "awcc": look, "result": a }), &t)
}

fn lighting_import(ctx: &mut Ctx, dry_run: bool) -> R {
    ctx.lights_here()?;
    let path = ctx.settings_path();
    let mut all = Settings::load_from(&path);
    let mut io = ctx.light_io();
    let look = awcc_look(io.as_mut())?;
    let l = lightctl::import_awcc(&all.lighting, &look);
    l.validate().map_err(Fail::Refused)?;
    let t = format!("Seeded from AWCC's \"{}\": base {}, {} key colour(s). Nothing written.", look.preset, l.keyboard.color, l.keyboard.keys.len());
    if dry_run {
        let (v, t) = dry_run_doc(&l, &[], &[], &t);
        return ctx.emit(v, &t);
    }
    all.lighting = l.clone();
    all.save_to(&path).map_err(Fail::Error)?;
    let t = format!("{t} Saved to {}; `cockpit lighting mode custom` writes them.", path.display());
    ctx.emit(json!({ "settings": l, "saved": path, "applied": false }), &t)
}

// ---------------------------------------------------------------- the reactive glow

/// The command line's clock, for the glow's loop.
struct GlowClock<'a>(&'a mut dyn Clock);

impl reactive::Clock for GlowClock<'_> {
    fn now_ms(&mut self) -> u64 {
        self.0.now_ms()
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.0.sleep_ms(ms)
    }
    fn stopped(&self) -> bool {
        self.0.stopped()
    }
}

/// A dry run's clock: simulated time, no sleeping.
#[derive(Default)]
struct SimClock(u64);

impl reactive::Clock for SimClock {
    fn now_ms(&mut self) -> u64 {
        self.0
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.0 += ms;
    }
}

/// What a dry run types, over and over, at [`DRY_KEYS_PER_S`].
const DRY_TEXT: &str = "the quick brown fox jumps over the lazy dog; fn main() { let x = 42; } ";
const DRY_KEYS_PER_S: u32 = 8;

fn per_s(reports: &[usize]) -> String {
    if reports.is_empty() {
        return "0".into();
    }
    format!("{:.0}", reports.iter().sum::<usize>() as f64 / reports.len() as f64)
}

/// `cockpit lighting reactive`: the glow in the foreground until Ctrl+C or `--seconds`, then the static
/// colours back. `--dry-run`: scripted typing (60 % of the time, then idle) on a fake keyboard, and the
/// report counts; nothing is saved or sent. The saved settings are used, never changed.
fn lighting_reactive(ctx: &mut Ctx, seconds: Option<u64>, dry_run: bool) -> R {
    let saved = Settings::load_from(&ctx.settings_path()).lighting;
    let power = PowerState::default();
    if dry_run {
        let mut l = saved;
        l.mode = Mode::Custom;
        l.keyboard.effect = KeyEffect::Static;
        l.keyboard.on = true;
        if l.keyboard.brightness == 0 {
            l.keyboard.brightness = 100;
        }
        l.reactive.on = true;
        l.validate().map_err(Fail::Refused)?;
        let secs = seconds.unwrap_or(10).max(1);
        let typing_ms = secs * 600;
        let text = DRY_TEXT.repeat((typing_ms / 1_000 * DRY_KEYS_PER_S as u64 / DRY_TEXT.len() as u64 + 1) as usize);
        let script: Vec<(u64, u8)> = reactive::script(&text, 0, DRY_KEYS_PER_S).into_iter().filter(|(t, _)| *t < typing_ms).collect();
        let mut next = 0;
        let io = lightctl::FakeIo::default();
        let sum = reactive::run(
            &mut io.clone(),
            &l,
            &power,
            Shown::of(&l, &power),
            None,
            &mut SimClock::default(),
            &mut |now| {
                let mut out = Vec::new();
                while next < script.len() && script[next].0 <= now {
                    out.push(script[next].1);
                    next += 1;
                }
                out
            },
            &mut |now| now >= secs * 1_000,
        )
        .map_err(Fail::Error)?;
        // The seconds that had key presses (rounded up), and the rest.
        let typing_s = typing_ms.div_ceil(1_000) as usize;
        let (typing, idle) = sum.per_second.split_at(typing_s.min(sum.per_second.len()));
        // The first second also carries the full frame: leave it out when there are later typing seconds.
        let typing = if typing.len() > 1 { &typing[1..] } else { typing };
        let st = &sum.stats;
        let r = &l.reactive;
        let mut t = format!("Dry run: nothing saved, nothing sent. Simulated {} key presses at {DRY_KEYS_PER_S} a second for {:.1} s, then idle to {secs} s, at {} fps, fade {} ms.\n", script.len(), typing_ms as f64 / 1_000.0, r.fps, r.decay_ms);
        t.push_str(&format!("First frame: {} reports (all 91 keys). Then {} frames with changes: {} reports, {} key colours, at most {} reports a frame.\n", st.first, st.frames, st.reports, st.keys, st.max_frame_reports));
        t.push_str(&format!("Reports each second: {}\n", sum.per_second.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" ")));
        t.push_str(&format!("About {} reports a second while typing, {} after it. Stopping writes the static colours back: {} reports.", per_s(typing), per_s(idle), sum.restore.keyboard));
        let v = json!({ "dryRun": true, "simulated": { "presses": script.len(), "keysPerSecond": DRY_KEYS_PER_S, "typingMs": typing_ms, "seconds": secs }, "settings": l.reactive, "summary": sum });
        return ctx.emit(v, &t);
    }
    if !cfg!(windows) {
        return Err(Fail::Refused("`cockpit lighting reactive` is Windows only: it reads key presses with Raw Input and writes the keyboard through Windows' HID driver. `--dry-run` simulates it here.".into()));
    }
    let mut l = saved;
    l.reactive.on = true;
    reactive::eligible(&l, &power).map_err(|e| Fail::Refused(format!("{e} (`cockpit lighting mode custom` and `cockpit lighting effect static` set it up)")))?;
    // Key presses only from the real keyboard: a test's devices get none.
    let listen = ctx.lights.is_none();
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    // Before anything else: AWCC resumed once, in case a crash left it paused (see `Pauser::recover`).
    pauser.recover();
    let (tx, rx) = std::sync::mpsc::channel::<u8>();
    #[cfg(windows)]
    if listen {
        std::thread::spawn(move || {
            let r = cockpit_core::keyinput::win::listen("CockpitReactiveGlow", move |make, flags| {
                if let Some(id) = cockpit_core::keyinput::led_for(make, flags) {
                    let _ = tx.send(id);
                }
            });
            if let Err(e) = r {
                eprintln!("key presses can't be read: {e:#}");
            }
        });
    }
    #[cfg(not(windows))]
    drop((tx, listen));
    if !ctx.json {
        let until = seconds.map_or("Ctrl+C stops it".to_string(), |s| format!("it stops after {s} s or on Ctrl+C"));
        writeln!(ctx.out, "Reactive glow on: keys warm up as you press them; {until} and the static colours come back.")?;
        ctx.out.flush()?;
    }
    ctx.clock.catch_stop();
    let start = ctx.clock.now_ms();
    let prev = shown_by(&l);
    let result = reactive::run(io.as_mut(), &l, &power, prev, l.pause_awcc.then_some(pauser.as_ref()), &mut GlowClock(&mut *ctx.clock), &mut |_| rx.try_iter().collect(), &mut |now| seconds.is_some_and(|s| now.saturating_sub(start) >= s * 1_000));
    let sum = result.map_err(Fail::Error)?;
    let st = &sum.stats;
    let t = format!("Reactive glow stopped after {:.1} s: {} frames, {} reports after the first {}. The static colours are back: {}", sum.ran_ms as f64 / 1_000.0, st.frames, st.reports, st.first, sum.restore.text());
    ctx.emit(json!({ "summary": sum }), &t)
}

// ---------------------------------------------------------------- the clock

/// `cockpit lighting clock`: the local time on the keys until Ctrl+C or `--seconds`, then the static colours
/// back. `--dry-run`: 10 simulated seconds on a fake keyboard, and the report counts.
fn lighting_clock(ctx: &mut Ctx, seconds: Option<u64>, base: &str, dry_run: bool) -> R {
    use cockpit_core::clock;
    let base = clock::ClockBase::parse(base).ok_or_else(|| Fail::Refused(format!("'{base}' isn't a base: use dim, off or preset")))?;
    let mut l = Settings::load_from(&ctx.settings_path()).lighting;
    let power = PowerState::default();
    if dry_run {
        l.mode = Mode::Custom;
        l.keyboard.on = true;
        if l.keyboard.brightness == 0 {
            l.keyboard.brightness = 100;
        }
        let secs = seconds.unwrap_or(10).max(1);
        let start = clock::local_now();
        let start_s = start.h as u64 * 3_600 + start.m as u64 * 60 + start.s as u64;
        let mut sim = SimClock::default();
        let mut t = 0u64;
        let io = lightctl::FakeIo::default();
        let sum = clock::run(&mut io.clone(), &l, &power, Shown::of(&l, &power), None, base, &mut sim, &mut || {
            t += clock::POLL_MS;
            clock::Hms::from_secs(start_s + t / 1_000)
        }, &mut |now| now >= secs * 1_000)
        .map_err(Fail::Error)?;
        let text = format!("Dry run: nothing saved, nothing sent. {secs} simulated seconds: first face {} reports, then {} changes, {} reports, {} key colours ({:.1} reports a second). Stopping writes the static colours back: {} reports.", sum.first, sum.frames, sum.reports, sum.keys, sum.reports as f64 / secs as f64, sum.restore.keyboard);
        return ctx.emit(json!({ "dryRun": true, "summary": sum }), &text);
    }
    ctx.lights_here()?;
    clock::eligible(&l, &power).map_err(|e| Fail::Refused(format!("{e} (`cockpit lighting mode custom` sets it up)")))?;
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    pauser.recover();
    if !ctx.json {
        let until = seconds.map_or("Ctrl+C stops it".to_string(), |s| format!("it stops after {s} s or on Ctrl+C"));
        writeln!(ctx.out, "Clock on: h:mm in big digits across the keyboard (hour peach, minutes cyan), a steady colon, the seconds in big pink digits for 5 s of every 15; {until} and the static colours come back.")?;
        ctx.out.flush()?;
    }
    ctx.clock.catch_stop();
    let start = ctx.clock.now_ms();
    let prev = shown_by(&l);
    let sum = clock::run(io.as_mut(), &l, &power, prev, l.pause_awcc.then_some(pauser.as_ref()), base, &mut GlowClock(&mut *ctx.clock), &mut clock::local_now, &mut |now| seconds.is_some_and(|s| now.saturating_sub(start) >= s * 1_000)).map_err(Fail::Error)?;
    let t = format!("Clock stopped after {:.1} s: {} changes, {} reports after the first {}. The static colours are back: {}", sum.ran_ms as f64 / 1_000.0, sum.frames, sum.reports, sum.first, sum.restore.text());
    ctx.emit(json!({ "summary": sum }), &t)
}

// ---------------------------------------------------------------- mouse paint

/// A dry run's pointer: a diagonal sweep over a 1920 x 1080 screen in 2 s, a colour per quarter of it.
struct SimPointer(u64);

impl mousepaint::PointerSource for SimPointer {
    fn screen(&mut self) -> mousepaint::Screen {
        mousepaint::Screen { x: 0, y: 0, w: 1920, h: 1080 }
    }
    fn position(&mut self) -> Option<(i32, i32)> {
        self.0 += 1;
        let f = ((self.0 * mousepaint::POLL_MS) % 2_000) as f64 / 2_000.0;
        Some(((f * 1919.0) as i32, (f * 1079.0) as i32))
    }
    fn color_at(&mut self, x: i32, _y: i32) -> Option<alienfx::Rgb> {
        let palette = ["#FF9480", "#00E5FF", "#A040FF", "#FFFFFF"];
        cockpit_core::lightctl::parse_rgb(palette[(x as usize * 4 / 1920).min(3)]).ok()
    }
}

/// `cockpit lighting mouse`: mouse paint until Ctrl+C or `--seconds`, then the static colours back.
/// `--dry-run`: a simulated sweep on a fake keyboard, and the report counts.
#[allow(clippy::too_many_arguments)]
fn lighting_mouse(ctx: &mut Ctx, seconds: Option<u64>, fps: u32, fade_ms: u64, dim: u32, look: &str, dry_run: bool) -> R {
    let (base, brush) = mousepaint::look(look).ok_or_else(|| Fail::Refused(format!("'{look}' isn't a look: use glow, uncover, reveal, boost or screen")))?;
    let mut l = Settings::load_from(&ctx.settings_path()).lighting;
    let power = PowerState::default();
    if dry_run {
        l.mode = Mode::Custom;
        l.keyboard.on = true;
        if l.keyboard.brightness == 0 {
            l.keyboard.brightness = 100;
        }
        let secs = seconds.unwrap_or(5).max(1);
        let io = lightctl::FakeIo::default();
        let sum = mousepaint::run(&mut io.clone(), &l, &power, Shown::of(&l, &power), None, base, brush, dim, fps, fade_ms, &mut SimClock::default(), &mut SimPointer(0), &mut |now| now >= secs * 1_000).map_err(Fail::Refused)?;
        let text = format!("Dry run: nothing saved, nothing sent. {secs} simulated seconds of a diagonal sweep at up to {fps} writes a second: first picture {} reports, then {} writes, {} reports, {} key colours ({:.1} reports a second). Stopping writes the static colours back: {} reports.", sum.first, sum.frames, sum.reports, sum.keys, sum.reports as f64 / secs as f64, sum.restore.keyboard);
        return ctx.emit(json!({ "dryRun": true, "summary": sum }), &text);
    }
    if !cfg!(windows) {
        return Err(Fail::Refused("`cockpit lighting mouse` is Windows only: it reads the mouse pointer and the screen through Windows. `--dry-run` simulates it here.".into()));
    }
    ctx.lights_here()?;
    mousepaint::eligible(&l, &power, fps, fade_ms, dim).map_err(|e| Fail::Refused(format!("{e} (`cockpit lighting mode custom` sets it up)")))?;
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    pauser.recover();
    #[cfg(windows)]
    let mut src: Box<dyn mousepaint::PointerSource> = Box::new(mousepaint::win::WinPointer::new());
    #[cfg(not(windows))]
    let mut src: Box<dyn mousepaint::PointerSource> = Box::new(SimPointer(0));
    if !ctx.json {
        let sc = src.screen();
        let until = seconds.map_or("Ctrl+C stops it".to_string(), |s| format!("it stops after {s} s or on Ctrl+C"));
        writeln!(ctx.out, "Mouse paint on: the screen ({} x {} pixels) is mapped onto the keyboard; look {look}, keys fade back over {:.1} s; {until} and the static colours come back.", sc.w, sc.h, fade_ms as f64 / 1_000.0)?;
        ctx.out.flush()?;
    }
    ctx.clock.catch_stop();
    let start = ctx.clock.now_ms();
    let prev = shown_by(&l);
    let sum = mousepaint::run(io.as_mut(), &l, &power, prev, l.pause_awcc.then_some(pauser.as_ref()), base, brush, dim, fps, fade_ms, &mut GlowClock(&mut *ctx.clock), src.as_mut(), &mut |now| seconds.is_some_and(|s| now.saturating_sub(start) >= s * 1_000)).map_err(Fail::Error)?;
    let t = format!("Mouse paint stopped after {:.1} s: {} writes, {} key colours, {} reports after the first {}. The static colours are back: {}", sum.ran_ms as f64 / 1_000.0, sum.frames, sum.keys, sum.reports, sum.first, sum.restore.text());
    ctx.emit(json!({ "summary": sum }), &t)
}

// ---------------------------------------------------------------- screen mirror

/// A dry run's screen: a colour band sweeping left to right every 2 s.
struct SimScreen(u64);

impl screenmirror::ScreenSource for SimScreen {
    fn size(&mut self) -> (i32, i32) {
        (1920, 1080)
    }
    fn grab(&mut self) -> Option<screenmirror::Grab> {
        use screenmirror::{GRAB_H, GRAB_W};
        self.0 += 1;
        let band = (self.0 as usize * 3) % GRAB_W;
        let px = (0..GRAB_W * GRAB_H).map(|i| if (i % GRAB_W).abs_diff(band) < 6 { alienfx::Rgb::new(0, 229, 255) } else { alienfx::Rgb::new(10, 10, 30) }).collect();
        Some(screenmirror::Grab { w: GRAB_W, h: GRAB_H, px })
    }
}

/// `cockpit lighting screen`: the screen mirror until Ctrl+C or `--seconds`, then the static colours back.
fn lighting_screen(ctx: &mut Ctx, seconds: Option<u64>, amb: screenmirror::Ambient, gpu: bool, dry_run: bool) -> R {
    let fps = amb.fps;
    let mut l = Settings::load_from(&ctx.settings_path()).lighting;
    let power = PowerState::default();
    if dry_run {
        l.mode = Mode::Custom;
        l.keyboard.on = true;
        if l.keyboard.brightness == 0 {
            l.keyboard.brightness = 100;
        }
        let secs = seconds.unwrap_or(5).max(1);
        let io = lightctl::FakeIo::default();
        let sum = screenmirror::run(&mut io.clone(), &l, &power, Shown::of(&l, &power), None, &amb, &mut SimClock::default(), &mut SimScreen(0), &mut |now| now >= secs * 1_000).map_err(Fail::Refused)?;
        let text = format!("Dry run: nothing saved, nothing sent. {secs} simulated seconds of a moving colour band at up to {fps} writes a second: first picture {} reports, then {} writes, {} reports, {} key colours ({:.1} reports a second). Stopping writes the static colours back: {} reports.", sum.first, sum.frames, sum.reports, sum.keys, sum.reports as f64 / secs as f64, sum.restore.keyboard);
        return ctx.emit(json!({ "dryRun": true, "summary": sum }), &text);
    }
    if !cfg!(windows) {
        return Err(Fail::Refused("`cockpit lighting screen` is Windows only: it reads the screen through Windows. `--dry-run` simulates it here.".into()));
    }
    ctx.lights_here()?;
    screenmirror::eligible(&l, &power, &amb).map_err(|e| Fail::Refused(format!("{e} (`cockpit lighting mode custom` sets it up)")))?;
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    pauser.recover();
    #[cfg(windows)]
    let mut src: Box<dyn screenmirror::ScreenSource> = screenmirror::open(gpu).map_err(Fail::Error)?;
    #[cfg(not(windows))]
    let mut src: Box<dyn screenmirror::ScreenSource> = {
        let _ = gpu;
        Box::new(SimScreen(0))
    };
    if !ctx.json {
        let (w, h) = src.size();
        let until = seconds.map_or("Ctrl+C stops it".to_string(), |s| format!("it stops after {s} s or on Ctrl+C"));
        let how = src.kind();
        writeln!(ctx.out, "Screen mirror on: the screen ({w} x {h} pixels, read by {how}) glows on the keyboard as one gradient, up to {fps} updates a second, spread {}, ease {} ms, saturation x{}; {until} and the static colours come back.", amb.spread, amb.smooth_ms, amb.saturation)?;
        ctx.out.flush()?;
    }
    ctx.clock.catch_stop();
    let start = ctx.clock.now_ms();
    let prev = shown_by(&l);
    let sum = screenmirror::run(io.as_mut(), &l, &power, prev, l.pause_awcc.then_some(pauser.as_ref()), &amb, &mut GlowClock(&mut *ctx.clock), src.as_mut(), &mut |now| seconds.is_some_and(|s| now.saturating_sub(start) >= s * 1_000)).map_err(Fail::Error)?;
    let t = format!("Screen mirror stopped after {:.1} s: {} screen reads (slowest {} ms), {} writes, {} key colours, {} reports after the first {}. The static colours are back: {}", sum.ran_ms as f64 / 1_000.0, sum.grabs, sum.max_grab_ms, sum.frames, sum.keys, sum.reports, sum.first, sum.restore.text());
    ctx.emit(json!({ "summary": sum }), &t)
}

// ---------------------------------------------------------------- live layers

struct LiveOverrides {
    effect: Option<String>,
    tap: Option<String>,
    tap_color: Option<String>,
    tap_speed: Option<u32>,
    mouse: Option<String>,
    display_off: bool,
}

/// `cockpit lighting live`: the app's layered live lighting in the foreground, from the saved settings with
/// this run's changes (nothing saved), until Ctrl+C or `--seconds`.
fn lighting_live(ctx: &mut Ctx, seconds: Option<u64>, o: LiveOverrides) -> R {
    use cockpit_core::live::{self, LiveKind, TapKind};
    let mut l = Settings::load_from(&ctx.settings_path()).lighting;
    if let Some(e) = o.effect {
        l.live.kind = if e == "none" { LiveKind::Off } else { LiveKind::parse(&e).unwrap_or_default() };
    }
    if let Some(t) = o.tap {
        l.live.tap = Vec::new();
        for part in t.split(',').map(str::trim).filter(|p| !p.is_empty() && *p != "none") {
            l.live.tap.push(TapKind::parse(part).ok_or_else(|| Fail::Refused(format!("'{part}' isn't a key-press effect: use laser, lightning, seismic or none")))?);
        }
    }
    if let Some(c) = o.tap_color {
        l.live.tap_color = c;
    }
    if let Some(v) = o.tap_speed {
        l.live.tap_speed = v;
    }
    if let Some(m) = o.mouse {
        if l.live.kind == LiveKind::Mouse {
            l.live.kind = LiveKind::Off;
        }
        l.live.mouse = m;
    }
    let power = PowerState { display_off: o.display_off, ..Default::default() };
    if o.display_off {
        l.clock_on_display_off = true;
    }
    l.validate().map_err(Fail::Refused)?;
    if !cfg!(windows) {
        return Err(Fail::Refused("`cockpit lighting live` is Windows only: it reads the clock, the pointer and the screen through Windows.".into()));
    }
    ctx.lights_here()?;
    let plan = live::plan(&l, &power).ok_or_else(|| Fail::Refused("nothing to run: choose the clock or screen effect, a key-press effect or a mouse interaction (and custom lighting, keyboard on, Static on the keyboard)".into()))?;
    // Key presses for the laser: read-only Raw Input on its own thread, LED ids only.
    let (tx, rx) = std::sync::mpsc::channel::<u8>();
    #[cfg(windows)]
    if plan.needs_keys() && ctx.lights.is_none() {
        std::thread::spawn(move || {
            let r = cockpit_core::keyinput::win::listen("CockpitLiveKeys", move |make, flags| {
                if let Some(id) = cockpit_core::keyinput::led_for(make, flags) {
                    let _ = tx.send(id);
                }
            });
            if let Err(e) = r {
                eprintln!("key presses can't be read: {e:#}");
            }
        });
    }
    #[cfg(not(windows))]
    drop(tx);
    let mut presses = || rx.try_iter().collect::<Vec<u8>>();
    let mut io = ctx.light_io();
    let pauser = io.pauser();
    pauser.recover();
    if !ctx.json {
        let until = seconds.map_or("Ctrl+C stops it".to_string(), |s| format!("it stops after {s} s or on Ctrl+C"));
        writeln!(ctx.out, "Live lighting on: {plan:?}; {until} and the static colours come back.")?;
        ctx.out.flush()?;
    }
    ctx.clock.catch_stop();
    let start = ctx.clock.now_ms();
    let prev = shown_by(&l);
    let text = live::run_windows(io.as_mut(), &l, &power, prev, l.pause_awcc.then_some(pauser.as_ref()), &mut GlowClock(&mut *ctx.clock), Some(&mut presses), &mut |now| seconds.is_some_and(|s| now.saturating_sub(start) >= s * 1_000)).map_err(Fail::Error)?;
    ctx.emit(json!({ "text": text }), &text)
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
