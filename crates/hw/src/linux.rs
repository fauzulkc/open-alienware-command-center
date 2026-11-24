//! Linux backend: the kernel's `alienware-wmi` driver (its AWCC "WMAX" part, `alienware-wmi-wmax.c`) through
//! sysfs, implementing [`Machine`] with the same ids, gates and fault rules as the Windows AWCC path.
//!
//! Written in plain `std::fs` against a filesystem root, so the tests run on any system against a fake tree.
//!
//! # Sources (read 2026-11-22)
//!
//! - Driver guide: <https://docs.kernel.org/admin-guide/laptops/alienware-wmi.html>
//! - WMAX interface (ops, profile ids): <https://www.kernel.org/doc/html/latest/wmi/devices/alienware-wmi.html>
//! - platform_profile ABI: <https://docs.kernel.org/userspace-api/sysfs-platform_profile.html>
//! - hwmon ABI: <https://docs.kernel.org/hwmon/sysfs-interface.html>
//! - `fanY_boost` ABI (KernelVersion 6.15): <https://kernel.org/doc/Documentation/ABI/testing/sysfs-platform-alienware-wmi>
//! - Driver source: <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/platform/x86/dell/alienware-wmi-wmax.c>
//!
//! # What the driver exposes
//!
//! - **Thermal profile**: a platform-profile class device named `alienware-wmi`, found as
//!   `/sys/class/platform-profile/platform-profile-*/` whose `name` reads `alienware-wmi`, with `choices`
//!   (space separated) and `profile`. The legacy `/sys/firmware/acpi/platform_profile` (+ `_choices`) is the
//!   aggregate of every profile driver; it is used only when the class device is missing. Reading `profile`
//!   asks the firmware for its current profile (op 0x0B) and maps it back, so it is a real read-back.
//! - **Choices** come from the profile ids the firmware lists (op 0x03), mapped by `awcc_profile_to_pprof`;
//!   `custom` (0x00) is always added. G-Mode (0xAB) is skipped from the list and reached only through a
//!   quirk: with `gmode` set (the G-series quirk, or `alienware_wmi.force_gmode=1`) `performance` becomes
//!   0xAB and the driver toggles Game Shift with it. The `Alienware x17` DMI entry (which also matches
//!   "Alienware x17 R2") uses the generic quirks, **without G-Mode**: there `performance` is 0xA4.
//! - **Readings**: an hwmon device named `alienware_wmi` (`/sys/class/hwmon/hwmon*/name`). `tempN_input`
//!   (millidegrees C) and `tempN_label` (`CPU` 0x01, `Front` 0x03, `GPU` 0x06, else `Unknown`), channels in
//!   ascending sensor id; `fanN_input` (RPM), `fanN_label` (`CPU Fan` for 0x32/0x3B, `GPU Fan` for
//!   0x33/0x3C, ...), `fanN_min`/`fanN_max` (RPM, read once at probe), channels in the firmware's resource
//!   order; `pwmN_auto_channels_temp`, a bitmask of the temp channels each fan follows (bit k = `temp{k+1}`).
//! - **Fan boost**: `fanN_boost` in the same hwmon directory (mode 0644, kernel 6.15+), not debugfs, not
//!   `pwmN`. It writes the raw firmware boost (Thermal_Control op 0x02), clamped to 0..=255. The driver docs
//!   note that on some models manual fan control only works reliably in the `custom` profile.
//! - **Root**: hwmon readings and `profile` are world-readable; writing `profile` and `fanN_boost` needs root
//!   (standard sysfs 0644). debugfs (`/sys/kernel/debug/alienware-wmi-*/pprof_data`) is root-only.
//!
//! # Profile mapping
//!
//! | platform_profile choice | AWCC id | AWCC name |
//! |---|---|---|
//! | `low-power` | 0xA5 | Battery Saver |
//! | `cool` | 0xA2 | Cool |
//! | `quiet` | 0xA3 | Quiet |
//! | `balanced` | 0xA0 | Balanced |
//! | `balanced-performance` | 0xA1 | Performance |
//! | `performance` (driver without G-Mode: the x17's default) | 0xA4 | Full Speed |
//! | `performance` (driver with G-Mode) | 0xAB | G-Mode |
//! | `custom` | 0x00 | Custom: read only, never offered |
//!
//! Only choices the class device lists are offered. Whether `performance` means G-Mode is read, in order,
//! from debugfs `pprof_data` (an `ID: 0xab` line; needs root), `alienware_wmi.force_gmode` on
//! `/proc/cmdline`, or an `options alienware_wmi force_gmode=` line in `/etc/modprobe.d/*.conf`; otherwise
//! it is Full Speed, as the driver's DMI table gives the x17. With G-Mode on, Full Speed can't be reached.
//!
//! # Sensor and fan mapping
//!
//! | hwmon label | AWCC id |
//! |---|---|
//! | `tempN_label` `CPU` / `Front` / `GPU` | 0x01 / 0x03 / 0x06 (`Unknown`: skipped) |
//! | `fanN_label` `CPU Fan`, 1st / 2nd seen | 0x32 / 0x3B |
//! | `GPU Fan`, 1st / 2nd | 0x33 / 0x3C |
//! | `PCI Fan`, `Mid Fan`, `Side Fan` | 0x34, 0x35, 0x37 |
//! | `Top Fan` 1st..3rd, `U.2 Fan` 1st..2nd | 0x36, 0x3D, 0x3E; 0x38, 0x39 |
//! | `Front Fan` 1st..2nd, `Bottom Fan` 1st..2nd | 0x3A, 0x3F; 0x40, 0x41 |
//!
//! Labels shared by two ids are told apart by order: the driver lists fans in the firmware's resource order,
//! which on the x17 R2 is 0x32, 0x33, 0x3B, 0x3C. When a label file is missing, the channel is mapped by
//! index to that x17 R2 order (`temp1` 0x01, `temp2` 0x06; `fan1..4` 0x32, 0x33, 0x3B, 0x3C).
//! Each fan's sensor comes from the lowest bit of `pwmN_auto_channels_temp`; without it, CPU fans follow
//! 0x01 and GPU fans 0x06 (what the x17 R2's GetFanSensors reports).
//!
//! # Boost scale
//!
//! The driver's 0..=255 is the firmware's raw boost, the same unit the Windows path writes. Values above 100
//! are uncalibrated "overboost" (see [`MAX_BOOST`]), so 0..=100 is written 1:1 and nothing above 100 is ever
//! written.

use crate::control::{verified, BoostChange, ModeChange, EMERGENCY_BOOST, MAX_BOOST};
use crate::machine::Machine;
use crate::reader::{sensor_name, Alert, FanReading, SensorReading, Snapshot, STALE_MS};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wiregent_oem_alienware::rules::{Emergency, FanPairWatch, RateLimit, Sample, StuckWatch, ZoneStoppedWatch};
use wiregent_oem_alienware::{decode_rpm, decode_temp, profile_name, Discovery, SystemDescription, GMODE};

/// The platform-profile class device's `name`.
pub const PLATFORM_PROFILE_NAME: &str = "alienware-wmi";
/// The hwmon device's `name`.
pub const HWMON_NAME: &str = "alienware_wmi";

/// x17 R2 order of temperature channels, for channels without a label.
const X17_TEMPS: [u8; 2] = [0x01, 0x06];
/// x17 R2 order of fan channels (resource order), for channels without a label.
const X17_FANS: [u8; 4] = [0x32, 0x33, 0x3B, 0x3C];

/// A platform_profile choice as an AWCC id. `performance` is G-Mode when the driver maps it there.
pub fn choice_to_awcc(choice: &str, performance_is_gmode: bool) -> Option<u8> {
    Some(match choice.trim() {
        "low-power" => 0xA5,
        "cool" => 0xA2,
        "quiet" => 0xA3,
        "balanced" => 0xA0,
        "balanced-performance" => 0xA1,
        "performance" if performance_is_gmode => GMODE,
        "performance" => 0xA4,
        "custom" => 0x00,
        _ => return None,
    })
}

/// An AWCC id as the platform_profile choice that selects it, given how the driver maps `performance`.
pub fn awcc_to_choice(id: u8, performance_is_gmode: bool) -> Option<&'static str> {
    Some(match id {
        0xA5 => "low-power",
        0xA2 => "cool",
        0xA3 => "quiet",
        0xA0 => "balanced",
        0xA1 => "balanced-performance",
        0xA4 if !performance_is_gmode => "performance",
        GMODE if performance_is_gmode => "performance",
        0x00 => "custom",
        _ => return None,
    })
}

/// The AWCC id a `tempN_label` stands for.
fn temp_label_id(label: &str) -> Option<u8> {
    match label.trim() {
        "CPU" => Some(0x01),
        "Front" => Some(0x03),
        "GPU" => Some(0x06),
        _ => None,
    }
}

/// The AWCC ids a `fanN_label` can stand for, in the order the firmware lists them.
fn fan_label_ids(label: &str) -> &'static [u8] {
    match label.trim() {
        "CPU Fan" => &[0x32, 0x3B],
        "GPU Fan" => &[0x33, 0x3C],
        "PCI Fan" => &[0x34],
        "Mid Fan" => &[0x35],
        "Top Fan" => &[0x36, 0x3D, 0x3E],
        "Side Fan" => &[0x37],
        "U.2 Fan" => &[0x38, 0x39],
        "Front Fan" => &[0x3A, 0x3F],
        "Bottom Fan" => &[0x40, 0x41],
        _ => &[],
    }
}

/// The sensor a fan follows when the driver gives no `pwmN_auto_channels_temp`.
fn default_sensor(fan: u8) -> Option<u8> {
    match fan {
        0x32 | 0x3B => Some(0x01),
        0x33 | 0x3C => Some(0x06),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct TempChannel {
    id: u8,
    n: u32,
}

#[derive(Debug, Clone)]
struct FanChannel {
    id: u8,
    n: u32,
    max_rpm: Option<u32>,
}

pub struct LinuxSysfs {
    model: String,
    bios: String,
    /// (`profile`, `choices`) files.
    pp: Option<(PathBuf, PathBuf)>,
    performance_is_gmode: bool,
    profiles: Vec<u8>,
    hwmon: Option<PathBuf>,
    temps: Vec<TempChannel>,
    fan_channels: Vec<FanChannel>,
    discovery: Discovery,
    notes: Vec<String>,
    limit: RateLimit,
    wait: fn(Duration),
    fan_pair: FanPairWatch,
    stuck: StuckWatch,
    zone: ZoneStoppedWatch,
    last_valid: BTreeMap<u8, u64>,
    stale_reported: BTreeMap<u8, bool>,
    started_ms: Option<u64>,
}

fn read_trim(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_num(p: &Path) -> Option<i64> {
    read_trim(p)?.parse().ok()
}

/// Every `<prefix>N_input` channel number in `dir`, ascending.
fn channels(dir: &Path, prefix: &str) -> Vec<u32> {
    let mut ns: Vec<u32> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_prefix(prefix)?.strip_suffix("_input")?.parse().ok())
        .filter(|n| *n > 0)
        .collect();
    ns.sort_unstable();
    ns
}

/// The directory under `class` whose `name` file reads `name`.
fn find_named(class: &Path, name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(class).ok()?.flatten().map(|e| e.path()).collect();
    dirs.sort();
    dirs.into_iter().find(|d| read_trim(&d.join("name")).as_deref() == Some(name))
}

/// A kernel boolean parameter value (`kstrtobool`).
fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "y" | "yes" | "t" | "true" | "on")
}

fn gmode_note(on: bool, source: &str) -> String {
    format!("`performance` is {} ({source})", if on { "G-Mode, 0xAB" } else { "Full Speed, 0xA4" })
}

/// Whether the driver maps `performance` to G-Mode, and where that was learned.
fn detect_gmode(root: &Path) -> (bool, String) {
    let mut debug: Vec<PathBuf> = fs::read_dir(root.join("sys/kernel/debug"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("alienware-wmi"))
        .map(|e| e.path().join("pprof_data"))
        .collect();
    debug.sort();
    for p in debug {
        if let Ok(text) = fs::read_to_string(&p) {
            let on = text.lines().any(|l| l.trim().eq_ignore_ascii_case("ID: 0xab"));
            return (on, gmode_note(on, "debugfs pprof_data"));
        }
    }
    if let Some(cmdline) = read_trim(&root.join("proc/cmdline")) {
        for arg in cmdline.split_whitespace() {
            if let Some(v) = arg.strip_prefix("alienware_wmi.force_gmode=").or_else(|| arg.strip_prefix("alienware-wmi.force_gmode=")) {
                return (truthy(v), gmode_note(truthy(v), "kernel command line"));
            }
        }
    }
    let mut confs: Vec<PathBuf> = fs::read_dir(root.join("etc/modprobe.d")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "conf")).collect();
    confs.sort();
    for conf in confs {
        let Ok(text) = fs::read_to_string(&conf) else { continue };
        for line in text.lines() {
            let mut words = line.split_whitespace();
            if words.next() != Some("options") || !matches!(words.next(), Some("alienware_wmi" | "alienware-wmi")) {
                continue;
            }
            if let Some(v) = words.find_map(|w| w.strip_prefix("force_gmode=")) {
                return (truthy(v), gmode_note(truthy(v), &conf.display().to_string()));
            }
        }
    }
    (false, gmode_note(false, "the driver's x17 entry has no G-Mode and nothing forces it"))
}

impl LinuxSysfs {
    /// The real system.
    pub fn open_default() -> Result<Self> {
        Self::open(Path::new("/"))
    }

    /// Reads identity, the profile device and the hwmon channels once, under `root` (`/` on a real system).
    pub fn open(root: &Path) -> Result<Self> {
        let dmi = root.join("sys/class/dmi/id");
        let model = read_trim(&dmi.join("product_name")).unwrap_or_default();
        let bios = read_trim(&dmi.join("bios_version")).unwrap_or_default();
        let mut notes = Vec::new();

        let pp = match find_named(&root.join("sys/class/platform-profile"), PLATFORM_PROFILE_NAME) {
            Some(d) => Some((d.join("profile"), d.join("choices"))),
            None => {
                let acpi = root.join("sys/firmware/acpi");
                let legacy = (acpi.join("platform_profile"), acpi.join("platform_profile_choices"));
                legacy.1.is_file().then(|| {
                    notes.push("no `alienware-wmi` platform-profile device; using the legacy aggregate /sys/firmware/acpi/platform_profile".to_string());
                    legacy
                })
            }
        };
        let (performance_is_gmode, how) = detect_gmode(root);
        notes.push(how);
        let mut profiles = Vec::new();
        if let Some((_, choices)) = &pp {
            for c in read_trim(choices).unwrap_or_default().split_whitespace() {
                // Custom (0x00) is the driver's own addition, not an AWCC mode; unknown choices aren't offered.
                if let Some(id) = choice_to_awcc(c, performance_is_gmode).filter(|id| *id != 0x00 && !profiles.contains(id)) {
                    profiles.push(id);
                }
            }
        }

        let hwmon = find_named(&root.join("sys/class/hwmon"), HWMON_NAME);
        let (mut temps, mut fan_channels, mut fan_sensor) = (Vec::<TempChannel>::new(), Vec::<FanChannel>::new(), BTreeMap::new());
        if let Some(h) = &hwmon {
            for n in channels(h, "temp") {
                let id = match read_trim(&h.join(format!("temp{n}_label"))) {
                    Some(label) => temp_label_id(&label),
                    None => {
                        notes.push(format!("temp{n} has no label; mapped by x17 R2 index"));
                        X17_TEMPS.get(n as usize - 1).copied()
                    }
                };
                match id.filter(|id| !temps.iter().any(|t| t.id == *id)) {
                    Some(id) => temps.push(TempChannel { id, n }),
                    None => notes.push(format!("temp{n} skipped: no AWCC id for it")),
                }
            }
            let mut seen: BTreeMap<String, usize> = BTreeMap::new();
            for n in channels(h, "fan") {
                let id = match read_trim(&h.join(format!("fan{n}_label"))) {
                    Some(label) => {
                        let k = seen.entry(label.clone()).or_default();
                        *k += 1;
                        fan_label_ids(&label).get(*k - 1).copied()
                    }
                    None => {
                        notes.push(format!("fan{n} has no label; mapped by x17 R2 index"));
                        X17_FANS.get(n as usize - 1).copied()
                    }
                };
                let Some(id) = id.filter(|id| !fan_channels.iter().any(|f| f.id == *id)) else {
                    notes.push(format!("fan{n} skipped: no AWCC id for it"));
                    continue;
                };
                let max_rpm = read_num(&h.join(format!("fan{n}_max"))).filter(|v| *v > 0).and_then(|v| u32::try_from(v).ok());
                // Bit k of auto_channels_temp is the k-th temp channel the driver lists (temp{k+1}).
                let sensor = match read_num(&h.join(format!("pwm{n}_auto_channels_temp"))).filter(|b| *b > 0) {
                    Some(bits) => temps.iter().find(|t| t.n == bits.trailing_zeros() + 1).map(|t| t.id),
                    None => default_sensor(id).filter(|s| temps.iter().any(|t| t.id == *s)),
                };
                if let Some(s) = sensor {
                    fan_sensor.insert(id, s);
                }
                fan_channels.push(FanChannel { id, n, max_rpm });
            }
        }
        if pp.is_none() && hwmon.is_none() {
            bail!("the alienware-wmi driver isn't loaded or doesn't support this machine: no `{PLATFORM_PROFILE_NAME}` platform profile and no `{HWMON_NAME}` hwmon device under {}", root.join("sys").display());
        }
        let fans: Vec<u8> = fan_channels.iter().map(|f| f.id).collect();
        let sensors: Vec<u8> = temps.iter().map(|t| t.id).collect();
        let description = SystemDescription { fans: fans.len() as u8, sensors: sensors.len() as u8, unknown: 0, profiles: profiles.len() as u8 };
        let discovery = Discovery { description, fans, sensors, profiles: profiles.clone(), fan_sensor };
        Ok(LinuxSysfs {
            model,
            bios,
            pp,
            performance_is_gmode,
            profiles,
            hwmon,
            temps,
            fan_channels,
            discovery,
            notes,
            limit: RateLimit::default(),
            wait: std::thread::sleep,
            fan_pair: FanPairWatch::default(),
            stuck: StuckWatch::default(),
            zone: ZoneStoppedWatch::default(),
            last_valid: BTreeMap::new(),
            stale_reported: BTreeMap::new(),
            started_ms: None,
        })
    }

    /// Replaces the sleep between read-backs (tests: a no-op).
    pub fn with_wait(mut self, wait: fn(Duration)) -> Self {
        self.wait = wait;
        self
    }

    /// The fans, sensors and fan→sensor map in the firmware path's shape (for the rules and `lists`).
    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// How the mapping was made (labels or index, where the G-Mode answer came from), for `info`.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Whether `performance` selects G-Mode (0xAB) on this driver rather than Full Speed (0xA4).
    pub fn performance_is_gmode(&self) -> bool {
        self.performance_is_gmode
    }

    /// After sleep: staleness clocks and the fault rules start over, as the Windows reader does.
    pub fn resumed(&mut self) {
        self.last_valid.clear();
        self.stale_reported.clear();
        self.started_ms = None;
        self.fan_pair = FanPairWatch::default();
        self.stuck = StuckWatch::default();
        self.zone = ZoneStoppedWatch::default();
    }

    fn machine_ok(&self) -> bool {
        verified(&self.model, &self.bios)
    }

    fn current_profile(&self) -> Option<u8> {
        let (profile, _) = self.pp.as_ref()?;
        choice_to_awcc(&read_trim(profile)?, self.performance_is_gmode)
    }

    fn fan(&self, id: u8) -> Option<&FanChannel> {
        self.fan_channels.iter().find(|f| f.id == id)
    }

    /// The `fanN_boost` file of a reported fan, or why there is none.
    fn boost_file(&self, fan: u8) -> Result<PathBuf> {
        let ch = self.fan(fan).ok_or_else(|| anyhow!("fan 0x{fan:02X} isn't one this machine reported"))?;
        let h = self.hwmon.as_ref().ok_or_else(|| anyhow!("no `{HWMON_NAME}` hwmon device"))?;
        let p = h.join(format!("fan{}_boost", ch.n));
        if !p.is_file() {
            bail!("the alienware-wmi driver offers no fan boost for fan 0x{fan:02X} here: {} is missing (fanN_boost arrived in kernel 6.15; an older kernel, or a driver without AWCC hwmon support on this model, can't set fan boost)", p.display());
        }
        Ok(p)
    }

    fn read_boost(&self, fan: u8) -> Option<u32> {
        read_num(&self.boost_file(fan).ok()?).and_then(|v| u32::try_from(v).ok())
    }

    fn read_rpm(&self, ch: &FanChannel) -> Option<u32> {
        let v = read_num(&self.hwmon.as_ref()?.join(format!("fan{}_input", ch.n)))?;
        decode_rpm(u32::try_from(v).ok()?, ch.max_rpm)
    }

    fn read_temp(&self, ch: &TempChannel) -> Option<i32> {
        let milli = read_num(&self.hwmon.as_ref()?.join(format!("temp{}_input", ch.n)))?;
        if milli < 0 {
            return None;
        }
        decode_temp(u32::try_from((milli + 500) / 1000).ok()?)
    }

    /// Writes `level` to a fan's boost file. No gates here: the callers hold them.
    fn write_boost(&self, fan: u8, level: u8) -> Result<()> {
        write_sysfs(&self.boost_file(fan)?, &level.to_string())
    }
}

fn write_sysfs(p: &Path, value: &str) -> Result<()> {
    fs::write(p, value).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => anyhow!("writing {} needs root (or a udev rule that grants write access): {e}", p.display()),
        _ => anyhow!("writing {}: {e}", p.display()),
    })
}

fn name(p: Option<u8>) -> Option<String> {
    p.map(|p| profile_name(p).to_string())
}

impl Machine for LinuxSysfs {
    fn backend(&self) -> &'static str {
        "linux-sysfs"
    }

    fn identity(&self) -> (String, String) {
        (self.model.clone(), self.bios.clone())
    }

    /// The same steps as `reader::Reader::tick`: values checked by the shared decoders, staleness after
    /// 30 s, then the fan-pair, stuck-reading and zone-stopped rules on the same sample.
    fn snapshot(&mut self, t_ms: u64) -> Snapshot {
        let started = *self.started_ms.get_or_insert(t_ms);
        let mut alerts = Vec::new();
        let mut temps = BTreeMap::new();
        let mut sensors = Vec::new();
        for ch in &self.temps {
            let s = ch.id;
            let c = self.read_temp(ch);
            if let Some(v) = c {
                temps.insert(s, v);
                self.last_valid.insert(s, t_ms);
                self.stale_reported.insert(s, false);
            }
            let stale = t_ms.saturating_sub(*self.last_valid.get(&s).unwrap_or(&started)) >= STALE_MS;
            if stale && !self.stale_reported.get(&s).copied().unwrap_or(false) {
                self.stale_reported.insert(s, true);
                alerts.push(Alert::SensorStale { sensor: s, name: sensor_name(s) });
            }
            sensors.push(SensorReading { id: s, name: sensor_name(s), celsius: c, stale });
        }
        let mut rpm = BTreeMap::new();
        let mut fans = Vec::new();
        for ch in &self.fan_channels {
            let r = self.read_rpm(ch);
            if let Some(v) = r {
                rpm.insert(ch.id, v);
            }
            fans.push(FanReading { id: ch.id, sensor: self.discovery.fan_sensor.get(&ch.id).copied(), rpm: r, max_rpm: ch.max_rpm });
        }
        let profile = self.current_profile();
        let sample = Sample { t_ms, temps, rpm };
        alerts.extend(self.fan_pair.push(&self.discovery, &sample).into_iter().map(Alert::FanFault));
        alerts.extend(self.stuck.push(&self.discovery, &sample).into_iter().map(Alert::StuckReading));
        alerts.extend(self.zone.push(&self.discovery, &sample).into_iter().map(Alert::ZoneStopped));
        Snapshot { t_ms, sensors, fans, profile, profile_name: profile.map(profile_name), alerts }
    }

    fn profiles(&self) -> Vec<u8> {
        self.profiles.clone()
    }

    fn fans(&self) -> Vec<u8> {
        self.discovery.fans.clone()
    }

    fn set_profile(&mut self, profile: u8, now_ms: u64) -> Result<ModeChange> {
        if !self.machine_ok() {
            bail!("this laptop's model or BIOS isn't one the firmware map was verified on, so modes stay read-only");
        }
        if !self.profiles.contains(&profile) {
            bail!("profile 0x{profile:02X} isn't one this machine reported (the driver lists {})", self.profiles.iter().map(|p| format!("0x{p:02X}")).collect::<Vec<_>>().join(", "));
        }
        let (Some((file, _)), Some(choice)) = (self.pp.clone(), awcc_to_choice(profile, self.performance_is_gmode)) else {
            bail!("profile 0x{profile:02X} has no platform_profile choice here");
        };
        let before = self.current_profile();
        if before == Some(profile) {
            return Ok(ModeChange { requested: profile_name(profile).into(), before: name(before), after: name(before), confirmed: true, unchanged: true });
        }
        if !self.limit.try_write(now_ms, false) {
            bail!("too many changes in a second; try again");
        }
        write_sysfs(&file, choice)?;
        let mut after = None;
        for _ in 0..12 {
            (self.wait)(Duration::from_millis(250));
            after = self.current_profile();
            if after == Some(profile) {
                break;
            }
        }
        Ok(ModeChange { requested: profile_name(profile).into(), before: name(before), after: name(after), confirmed: after == Some(profile), unchanged: false })
    }

    fn set_fan_boost(&mut self, fan: u8, level: u8, now_ms: u64) -> Result<BoostChange> {
        if !self.machine_ok() {
            bail!("this laptop's model or BIOS isn't one the firmware map was verified on, so fan boost stays off");
        }
        if level > MAX_BOOST {
            bail!("boost above {MAX_BOOST} (overboost) needs a per-fan calibration on this laptop first");
        }
        let ch = self.fan(fan).cloned().ok_or_else(|| anyhow!("fan 0x{fan:02X} isn't one this machine reported"))?;
        self.boost_file(fan)?;
        let before = self.read_boost(fan);
        let rpm_before = self.read_rpm(&ch);
        if before == Some(level as u32) {
            return Ok(BoostChange { fan: format!("0x{fan:02X}"), level, before, after: before, rpm_before, rpm_after: rpm_before, confirmed: true });
        }
        if !self.limit.try_write(now_ms, false) {
            bail!("too many changes in a second; try again");
        }
        // 0..=100 is written 1:1: the driver's 0..=255 is the firmware's raw boost (see the module docs).
        self.write_boost(fan, level)?;
        let mut after = None;
        for _ in 0..8 {
            (self.wait)(Duration::from_millis(250));
            after = self.read_boost(fan);
            if after == Some(level as u32) {
                break;
            }
        }
        // Give the fan a moment to respond, then read its speed again.
        (self.wait)(Duration::from_millis(1_500));
        let rpm_after = self.read_rpm(&ch);
        Ok(BoostChange { fan: format!("0x{fan:02X}"), level, before, after, rpm_before, rpm_after, confirmed: after == Some(level as u32) })
    }

    fn reset_fans(&mut self) -> Vec<String> {
        if !self.machine_ok() {
            return vec!["this laptop's model or BIOS isn't verified, so fan boost is never written here".into()];
        }
        let mut errors = Vec::new();
        for fan in self.discovery.fans.clone() {
            if self.read_boost(fan) == Some(0) {
                continue;
            }
            if let Err(e) = self.write_boost(fan, 0) {
                errors.push(format!("0x{fan:02X}: {e:#}"));
            }
        }
        errors
    }

    fn apply_emergency(&mut self, e: Emergency, person: &BTreeMap<u8, u8>) -> Vec<String> {
        if !self.machine_ok() {
            return vec!["this laptop's model or BIOS isn't verified, so the emergency rule can only warn".into()];
        }
        let mut errors = Vec::new();
        for fan in self.discovery.fans.clone() {
            let level = match e {
                Emergency::Engage => EMERGENCY_BOOST,
                Emergency::Release => person.get(&fan).copied().unwrap_or(0).min(MAX_BOOST),
            };
            if self.read_boost(fan) == Some(level as u32) {
                continue;
            }
            if let Err(err) = self.write_boost(fan, level) {
                errors.push(format!("0x{fan:02X}: {err:#}"));
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake sysfs tree shaped like the driver's on the x17 R2 (BIOS 1.32.0, kernel 6.15+), with a decoy
    /// profile driver and a decoy hwmon device so lookup by `name` is exercised.
    struct Tree {
        dir: tempfile::TempDir,
    }

    const HW: &str = "sys/class/hwmon/hwmon3";
    const PP: &str = "sys/class/platform-profile/platform-profile-1";

    impl Tree {
        fn x17() -> Self {
            let t = Tree { dir: tempfile::tempdir().unwrap() };
            t.put("sys/class/dmi/id/product_name", "Alienware x17 R2\n");
            t.put("sys/class/dmi/id/bios_version", "1.32.0\n");
            t.put("sys/class/platform-profile/platform-profile-0/name", "amd-pmf\n");
            t.put("sys/class/platform-profile/platform-profile-0/choices", "low-power balanced performance\n");
            t.put("sys/class/platform-profile/platform-profile-0/profile", "balanced\n");
            t.put(&format!("{PP}/name"), "alienware-wmi\n");
            // What the driver lists for the x17 R2's firmware ids A3 A0 A1 A4 A5 AB: G-Mode skipped, custom added.
            t.put(&format!("{PP}/choices"), "low-power quiet balanced balanced-performance performance custom\n");
            t.put(&format!("{PP}/profile"), "balanced\n");
            t.put("sys/class/hwmon/hwmon0/name", "acpitz\n");
            t.put("sys/class/hwmon/hwmon0/temp1_input", "45000\n");
            t.put(&format!("{HW}/name"), "alienware_wmi\n");
            for (n, label, milli) in [(1, "CPU", 100_000), (2, "GPU", 63_000)] {
                t.put(&format!("{HW}/temp{n}_label"), &format!("{label}\n"));
                t.put(&format!("{HW}/temp{n}_input"), &format!("{milli}\n"));
            }
            // Resource order 0x32, 0x33, 0x3B, 0x3C; auto_channels_temp bit 0 = temp1 (CPU), bit 1 = temp2 (GPU).
            for (n, label, rpm, max, bits) in [(1, "CPU Fan", 4600, 4200, 1), (2, "GPU Fan", 3000, 4100, 2), (3, "CPU Fan", 0, 3000, 1), (4, "GPU Fan", 3000, 3000, 2)] {
                t.put(&format!("{HW}/fan{n}_label"), &format!("{label}\n"));
                t.put(&format!("{HW}/fan{n}_input"), &format!("{rpm}\n"));
                t.put(&format!("{HW}/fan{n}_min"), "0\n");
                t.put(&format!("{HW}/fan{n}_max"), &format!("{max}\n"));
                t.put(&format!("{HW}/pwm{n}_auto_channels_temp"), &format!("{bits}\n"));
                t.put(&format!("{HW}/fan{n}_boost"), "0\n");
            }
            t
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }

        fn put(&self, rel: &str, text: &str) {
            let p = self.path(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        fn get(&self, rel: &str) -> String {
            fs::read_to_string(self.path(rel)).unwrap().trim().to_string()
        }

        fn remove(&self, rel: &str) {
            fs::remove_file(self.path(rel)).unwrap();
        }

        /// Every file and its bytes, to show a refused write changed nothing.
        fn contents(&self) -> BTreeMap<PathBuf, Vec<u8>> {
            fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
                for e in fs::read_dir(dir).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.insert(p.clone(), fs::read(&p).unwrap());
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(self.dir.path(), &mut out);
            out
        }

        fn open(&self) -> LinuxSysfs {
            LinuxSysfs::open(self.dir.path()).unwrap().with_wait(|_| {})
        }
    }

    #[test]
    fn snapshot_reads_awcc_ids_values_and_profile() {
        let t = Tree::x17();
        let mut m = t.open();
        assert_eq!(m.backend(), "linux-sysfs");
        assert_eq!(m.identity(), ("Alienware x17 R2".to_string(), "1.32.0".to_string()));
        assert_eq!(m.fans(), vec![0x32, 0x33, 0x3B, 0x3C]);
        let s = m.snapshot(0);
        assert_eq!((s.profile, s.profile_name), (Some(0xA0), Some("Balanced")));
        assert_eq!(s.sensors.iter().map(|x| (x.id, x.name.as_str(), x.celsius)).collect::<Vec<_>>(), vec![(0x01, "CPU", Some(100)), (0x06, "GPU", Some(63))]);
        let fan = |id| s.fans.iter().find(|f| f.id == id).unwrap().clone();
        assert_eq!((fan(0x32).rpm, fan(0x32).max_rpm, fan(0x32).sensor), (Some(4600), Some(4200), Some(0x01)));
        assert_eq!((fan(0x3B).rpm, fan(0x3B).sensor), (Some(0), Some(0x01)));
        assert_eq!((fan(0x33).sensor, fan(0x3C).sensor), (Some(0x06), Some(0x06)));
        assert!(s.alerts.is_empty());
        assert_eq!(m.discovery().lists()["fans"], vec!["0x32", "0x33", "0x3B", "0x3C"]);
    }

    #[test]
    fn profile_mapping_both_ways() {
        let table = [("low-power", 0xA5u8), ("cool", 0xA2), ("quiet", 0xA3), ("balanced", 0xA0), ("balanced-performance", 0xA1), ("custom", 0x00)];
        for gmode in [false, true] {
            for (choice, id) in table {
                assert_eq!(choice_to_awcc(choice, gmode), Some(id), "{choice}");
                assert_eq!(awcc_to_choice(id, gmode), Some(choice), "0x{id:02X}");
            }
        }
        assert_eq!(choice_to_awcc("performance", false), Some(0xA4), "the x17's driver: Full Speed");
        assert_eq!(choice_to_awcc("performance", true), Some(0xAB), "with G-Mode");
        assert_eq!((awcc_to_choice(0xA4, false), awcc_to_choice(0xAB, false)), (Some("performance"), None));
        assert_eq!((awcc_to_choice(0xAB, true), awcc_to_choice(0xA4, true)), (Some("performance"), None));
        assert_eq!(choice_to_awcc("max-power", false), None);
        assert_eq!(profile_name(choice_to_awcc("balanced-performance", false).unwrap()), "Performance");
    }

    #[test]
    fn only_listed_choices_are_offered() {
        let t = Tree::x17();
        assert_eq!(t.open().profiles(), vec![0xA5, 0xA3, 0xA0, 0xA1, 0xA4], "custom is never offered, G-Mode isn't mapped");
        t.put(&format!("{PP}/choices"), "balanced performance custom\n");
        assert_eq!(t.open().profiles(), vec![0xA0, 0xA4]);
    }

    #[test]
    fn g_mode_is_read_from_debugfs_or_the_command_line() {
        let t = Tree::x17();
        t.put("proc/cmdline", "quiet splash alienware_wmi.force_gmode=1\n");
        let m = t.open();
        assert!(m.performance_is_gmode());
        assert_eq!(m.profiles(), vec![0xA5, 0xA3, 0xA0, 0xA1, 0xAB], "performance is G-Mode; Full Speed can't be reached");
        t.put("sys/kernel/debug/alienware-wmi-A70591CE-A997-11DA-B012-B622A1EF5492/pprof_data", "Number of thermal profiles: 6\n\nPlatform profile 5:\n  ID: 0xa4\n");
        assert!(!t.open().performance_is_gmode(), "debugfs is the driver's own answer and wins");
    }

    #[test]
    fn set_profile_writes_the_choice_and_confirms_by_read_back() {
        let t = Tree::x17();
        let mut m = t.open();
        let r = m.set_profile(0xA1, 0).unwrap();
        assert_eq!(t.get(&format!("{PP}/profile")), "balanced-performance");
        assert_eq!((r.requested.as_str(), r.before.as_deref(), r.after.as_deref(), r.confirmed, r.unchanged), ("Performance", Some("Balanced"), Some("Performance"), true, false));
        let r = m.set_profile(0xA4, 1).unwrap();
        assert_eq!((t.get(&format!("{PP}/profile")).as_str(), r.after.as_deref(), r.confirmed), ("performance", Some("Full Speed"), true));
        let before = t.contents();
        let r = m.set_profile(0xA4, 2).unwrap();
        assert!(r.unchanged && r.confirmed);
        assert_eq!(t.contents(), before, "already set: nothing written");
    }

    #[test]
    fn an_unlisted_profile_is_refused_and_nothing_written() {
        let t = Tree::x17();
        let mut m = t.open();
        let before = t.contents();
        for p in [0xAB, 0xA2, 0x00, 0x42] {
            let e = m.set_profile(p, 0).unwrap_err().to_string();
            assert!(e.contains("isn't one this machine reported"), "{e}");
        }
        assert_eq!(t.contents(), before);
    }

    #[test]
    fn an_unverified_machine_is_refused_and_nothing_written() {
        let t = Tree::x17();
        t.put("sys/class/dmi/id/bios_version", "1.33.0\n");
        let mut m = t.open();
        let before = t.contents();
        assert!(m.set_profile(0xA3, 0).unwrap_err().to_string().contains("verified"));
        assert!(m.set_fan_boost(0x33, 50, 0).unwrap_err().to_string().contains("verified"));
        assert_eq!(m.apply_emergency(Emergency::Engage, &BTreeMap::new()).len(), 1);
        assert_eq!(m.reset_fans().len(), 1);
        assert_eq!(t.contents(), before);
        t.put("sys/class/dmi/id/product_name", "Alienware m16 R1\n");
        t.put("sys/class/dmi/id/bios_version", "1.32.0\n");
        let before = t.contents();
        assert!(t.open().set_profile(0xA3, 0).is_err(), "another model");
        assert_eq!(t.contents(), before);
    }

    #[test]
    fn boost_out_of_range_or_unknown_fan_is_refused_and_nothing_written() {
        let t = Tree::x17();
        let mut m = t.open();
        let before = t.contents();
        assert!(m.set_fan_boost(0x33, MAX_BOOST + 1, 0).unwrap_err().to_string().contains("overboost"));
        assert!(m.set_fan_boost(0x33, 255, 0).is_err());
        assert!(m.set_fan_boost(0x3D, 50, 0).unwrap_err().to_string().contains("isn't one this machine reported"));
        assert_eq!(t.contents(), before);
    }

    #[test]
    fn boost_is_written_one_to_one_and_confirmed() {
        let t = Tree::x17();
        let mut m = t.open();
        let r = m.set_fan_boost(0x33, 60, 0).unwrap();
        assert_eq!(t.get(&format!("{HW}/fan2_boost")), "60", "fan 0x33 is fan2; 60 written as 60");
        assert_eq!((r.fan.as_str(), r.before, r.after, r.rpm_before, r.confirmed), ("0x33", Some(0), Some(60), Some(3000), true));
        m.set_fan_boost(0x3B, MAX_BOOST, 1).unwrap();
        assert_eq!(t.get(&format!("{HW}/fan3_boost")), "100", "the highest ever written");
    }

    #[test]
    fn the_rate_limit_refuses_the_sixth_change_in_a_second() {
        let t = Tree::x17();
        let mut m = t.open();
        for (i, p) in [0xA3u8, 0xA1, 0xA4, 0xA5, 0xA0].iter().enumerate() {
            assert!(m.set_profile(*p, i as u64).unwrap().confirmed);
        }
        let before = t.contents();
        assert!(m.set_profile(0xA3, 6).unwrap_err().to_string().contains("too many changes"));
        assert!(m.set_fan_boost(0x32, 40, 7).unwrap_err().to_string().contains("too many changes"), "one budget for modes and fans");
        assert_eq!(t.contents(), before);
        assert!(m.set_profile(0xA3, 1_010).unwrap().confirmed, "a second later");
    }

    #[test]
    fn emergency_engages_past_the_rate_limit_and_releases_to_the_persons_boost() {
        let t = Tree::x17();
        let mut m = t.open();
        for i in 0..5 {
            m.set_fan_boost(0x33, 10 + i as u8, i).unwrap();
        }
        assert!(m.set_fan_boost(0x33, 30, 6).is_err(), "the person's budget is spent");
        assert!(m.apply_emergency(Emergency::Engage, &BTreeMap::new()).is_empty());
        for n in 1..=4 {
            assert_eq!(t.get(&format!("{HW}/fan{n}_boost")), EMERGENCY_BOOST.to_string(), "fan{n}");
        }
        let person = BTreeMap::from([(0x33u8, 60u8), (0x3C, 200)]);
        assert!(m.apply_emergency(Emergency::Release, &person).is_empty());
        let got: Vec<String> = (1..=4).map(|n| t.get(&format!("{HW}/fan{n}_boost"))).collect();
        assert_eq!(got, vec!["0", "60", "0", "100"], "fan2 = 0x33 back to 60; 0x3C capped at 100");
    }

    #[test]
    fn reset_puts_every_fan_back_to_zero() {
        let t = Tree::x17();
        let mut m = t.open();
        for (i, fan) in [0x32u8, 0x33, 0x3C].iter().enumerate() {
            m.set_fan_boost(*fan, 70, i as u64).unwrap();
        }
        assert!(m.reset_fans().is_empty());
        for n in 1..=4 {
            assert_eq!(t.get(&format!("{HW}/fan{n}_boost")), "0", "fan{n}");
        }
    }

    #[test]
    fn a_missing_boost_file_gives_a_clear_error() {
        let t = Tree::x17();
        for n in 1..=4 {
            t.remove(&format!("{HW}/fan{n}_boost"));
        }
        let mut m = t.open();
        assert_eq!(m.fans(), vec![0x32, 0x33, 0x3B, 0x3C], "fans are still listed");
        let before = t.contents();
        let e = m.set_fan_boost(0x33, 50, 0).unwrap_err().to_string();
        assert!(e.contains("no fan boost for fan 0x33") && e.contains("fan2_boost is missing") && e.contains("6.15"), "{e}");
        assert_eq!(m.reset_fans().len(), 4);
        let errs = m.apply_emergency(Emergency::Engage, &BTreeMap::new());
        assert_eq!(errs.len(), 4);
        assert!(errs[0].starts_with("0x32: ") && errs[0].contains("fan1_boost is missing"), "{errs:?}");
        assert_eq!(t.contents(), before);
        assert_eq!(m.snapshot(0).fans.len(), 4, "readings still work");
    }

    #[test]
    fn both_cpu_fans_stopped_while_hot_raises_the_zone_stopped_alert() {
        let t = Tree::x17();
        t.put(&format!("{HW}/fan1_input"), "0\n");
        let mut m = t.open();
        let mut fired = Vec::new();
        for i in 0..=40 {
            fired.extend(m.snapshot(i * 1_000).alerts);
        }
        assert_eq!(fired.len(), 1, "{fired:?}");
        let Alert::ZoneStopped(z) = &fired[0] else { panic!("{fired:?}") };
        assert_eq!((z.sensor, z.fans.clone(), z.from_ms, z.to_ms, z.peak_c), (0x01, vec![0x32, 0x3B], 0, 30_000, 100));
        assert!(fired[0].text().contains("Every CPU fan (0x32, 0x3B)"), "{}", fired[0].text());
        m.resumed();
        assert!(m.snapshot(41_000).alerts.is_empty(), "after sleep the episode starts over");
    }

    #[test]
    fn a_sensor_that_stops_answering_goes_stale_once() {
        let t = Tree::x17();
        let mut m = t.open();
        m.snapshot(0);
        t.remove(&format!("{HW}/temp1_input"));
        let s = m.snapshot(10_000);
        assert!(s.sensors[0].celsius.is_none() && !s.sensors[0].stale);
        let s = m.snapshot(31_000);
        assert!(s.sensors[0].stale && !s.sensors[1].stale);
        assert!(matches!(s.alerts.as_slice(), [Alert::SensorStale { sensor: 0x01, .. }]), "{:?}", s.alerts);
        assert!(m.snapshot(40_000).alerts.is_empty(), "once per episode");
    }

    #[test]
    fn without_labels_or_auto_channels_channels_map_by_x17_index() {
        let t = Tree::x17();
        for n in 1..=2 {
            t.remove(&format!("{HW}/temp{n}_label"));
        }
        for n in 1..=4 {
            t.remove(&format!("{HW}/fan{n}_label"));
            t.remove(&format!("{HW}/pwm{n}_auto_channels_temp"));
        }
        let mut m = t.open();
        assert_eq!(m.fans(), vec![0x32, 0x33, 0x3B, 0x3C]);
        assert_eq!(m.discovery().sensors, vec![0x01, 0x06]);
        assert_eq!(m.discovery().fan_sensor, BTreeMap::from([(0x32, 0x01), (0x33, 0x06), (0x3B, 0x01), (0x3C, 0x06)]));
        assert!(m.notes().iter().any(|n| n.contains("mapped by x17 R2 index")));
        assert_eq!(m.snapshot(0).sensors[0].celsius, Some(100));
    }

    #[test]
    fn no_driver_is_a_clear_error_and_the_legacy_profile_is_a_fallback() {
        let empty = tempfile::tempdir().unwrap();
        let e = LinuxSysfs::open(empty.path()).err().unwrap().to_string();
        assert!(e.contains("alienware-wmi driver isn't loaded"), "{e}");
        let t = Tree::x17();
        fs::remove_dir_all(t.path("sys/class/platform-profile")).unwrap();
        t.put("sys/firmware/acpi/platform_profile_choices", "quiet balanced performance\n");
        t.put("sys/firmware/acpi/platform_profile", "quiet\n");
        let mut m = t.open();
        assert_eq!(m.profiles(), vec![0xA3, 0xA0, 0xA4]);
        assert!(m.set_profile(0xA0, 0).unwrap().confirmed);
        assert_eq!(t.get("sys/firmware/acpi/platform_profile"), "balanced");
    }
}
