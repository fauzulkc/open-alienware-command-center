//! `wiregent-hw-probe`: H0 of docs/plans/plugins-v1.md. Read-only.
//!
//!   wiregent-hw-probe all   [--out DIR] [--watch SECS] [--keys SECS]   everything below, one elevated run
//!   wiregent-hw-probe awcc  [--out DIR]     firmware discovery + every read, compared with the 2026-10-07 capture
//!   wiregent-hw-probe watch [--out DIR] [--secs 120] [--every 2]   temperatures, fans, profile to CSV + fan check
//!   wiregent-hw-probe keys  [--out DIR] [--secs 60]   boost-key capture (press the key a few times)
//!   wiregent-hw-probe power [--out DIR]     CPU power and speed counters
//!   wiregent-hw-probe machine               manufacturer, model, BIOS
//!
//! Nothing here writes to the firmware: the probe only builds read calls and its WMI layer refuses the rest.

#[cfg(windows)]
mod keys;
#[cfg(windows)]
use wiregent_hw::{power, wmi};

#[cfg(not(windows))]
fn main() {
    eprintln!("wiregent-hw-probe runs on Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use crate::{keys, power, wmi};
    use anyhow::{bail, Context, Result};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use wiregent_oem_alienware::rules::{FanPairWatch, Sample};
    use wiregent_oem_alienware::*;

    struct Args {
        cmd: String,
        out: PathBuf,
        secs: Option<u64>,
        every: u64,
        watch: u64,
        keys: u64,
    }

    fn args() -> Result<Args> {
        let mut it = std::env::args().skip(1);
        let cmd = it.next().unwrap_or_else(|| "help".into());
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut a = Args { cmd, out: PathBuf::from(format!("h0-{stamp}")), secs: None, every: 2, watch: 120, keys: 60 };
        while let Some(flag) = it.next() {
            let mut val = || it.next().with_context(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--out" => a.out = PathBuf::from(val()?),
                "--secs" => a.secs = Some(val()?.parse()?),
                "--every" => a.every = val()?.parse::<u64>()?.max(1),
                "--watch" => a.watch = val()?.parse()?,
                "--keys" => a.keys = val()?.parse()?,
                other => bail!("unknown option {other}"),
            }
        }
        Ok(a)
    }

    fn is_admin() -> bool {
        unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
    }

    fn need_admin(what: &str) -> Result<()> {
        if !is_admin() {
            eprintln!("needs admin: {what} reads the Alienware firmware, which Windows only allows from an elevated prompt.");
            eprintln!("Open Terminal as administrator and run the same command again. Nothing was read.");
            std::process::exit(2);
        }
        Ok(())
    }

    fn write(dir: &Path, name: &str, text: &str) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(name), text).with_context(|| format!("writing {name}"))
    }

    pub fn run() -> Result<()> {
        let a = args()?;
        match a.cmd.as_str() {
            "machine" => {
                println!("{}", serde_json::to_string_pretty(&machine()?)?);
                Ok(())
            }
            "awcc" => {
                need_admin("awcc")?;
                let s = awcc(&a.out)?;
                println!("{s}");
                Ok(())
            }
            "watch" => {
                need_admin("watch")?;
                let s = watch(&a.out, a.secs.unwrap_or(120), a.every)?;
                println!("{s}");
                Ok(())
            }
            "keys" => {
                let s = keys_cmd(&a.out, a.secs.unwrap_or(60))?;
                println!("{s}");
                Ok(())
            }
            "power" => {
                let s = power_cmd(&a.out)?;
                println!("{s}");
                Ok(())
            }
            "all" => {
                need_admin("all")?;
                let mut summary = String::from("# H0 probe\n\n");
                let m = machine()?;
                let _ = writeln!(summary, "Machine: {} / {} / BIOS {}\n", m["manufacturer"], m["model"], m["bios"]);
                for (title, part) in [("Firmware", awcc(&a.out)), ("CPU power counters", power_cmd(&a.out))] {
                    let _ = writeln!(summary, "## {title}\n\n{}\n", part.unwrap_or_else(|e| format!("FAILED: {e:#}")));
                }
                println!("Watching temperatures and fans for {} s. Use the laptop normally; some load helps.", a.watch);
                let _ = writeln!(summary, "## Fans\n\n{}\n", watch(&a.out, a.watch, a.every).unwrap_or_else(|e| format!("FAILED: {e:#}")));
                println!("\nNow press the performance-boost key 5 times, a few seconds apart, over the next {} s.", a.keys);
                println!("Don't type: letters and digits are dropped anyway and only counted.");
                let _ = writeln!(summary, "## Boost key\n\n{}\n", keys_cmd(&a.out, a.keys).unwrap_or_else(|e| format!("FAILED: {e:#}")));
                write(&a.out, "summary.md", &summary)?;
                println!("\n{summary}\nEverything is in {}", a.out.display());
                Ok(())
            }
            _ => {
                println!("{}", include_str!("main.rs").lines().skip(2).take(8).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
                Ok(())
            }
        }
    }

    fn machine() -> Result<serde_json::Value> {
        let w = wmi::Wmi::connect("ROOT\\CIMV2")?;
        let cs = w.query("SELECT Manufacturer, Model FROM Win32_ComputerSystem", &["Manufacturer", "Model"])?;
        let bios = w.query("SELECT SMBIOSBIOSVersion FROM Win32_BIOS", &["SMBIOSBIOSVersion"])?;
        let f = |v: &Vec<Vec<String>>, i: usize| v.first().and_then(|r| r.get(i)).cloned().unwrap_or_default();
        // Parity checks (docs/plans/x17-parity.md): the audio stack, and whether Dell's BIOS-settings WMI exists
        // (battery charge modes need it).
        let sound: Vec<String> = w.query("SELECT Name, Manufacturer FROM Win32_SoundDevice", &["Name", "Manufacturer"]).unwrap_or_default().into_iter().map(|r| r.join(" / ")).collect();
        let bios_wmi = wmi::Wmi::connect("ROOT\\dcim\\sysman\\biosattributes").and_then(|b| b.query("SELECT AttributeName FROM EnumerationAttribute", &["AttributeName"])).map(|r| r.len()).ok();
        Ok(serde_json::json!({ "manufacturer": f(&cs, 0), "model": f(&cs, 1), "bios": f(&bios, 0), "admin": is_admin(), "sound": sound, "dellBiosWmiAttributes": bios_wmi }))
    }

    /// Every read call the PowerShell probe made, in the same order, plus discovery. Returns the summary text.
    fn awcc(out: &Path) -> Result<String> {
        let fw = wmi::Awcc::open()?;
        let mut lines = vec!["# wiregent-hw-probe awcc (read-only)".to_string(), format!("Instance: {}", fw.instance())];
        let mut rows: Vec<(String, u32, u32)> = Vec::new();
        let mut errors = Vec::new();
        let mut read = |c: Call, lines: &mut Vec<String>, rows: &mut Vec<(String, u32, u32)>| -> Option<u32> {
            let r = fw.read(c);
            let v = r.as_ref().ok().copied();
            lines.push(capture_line(&c, v));
            if let Some(v) = v {
                rows.push((c.method.wmi_name().to_string(), c.packed(), v));
            } else if let Err(e) = r {
                errors.push(format!("{}: {e:#}", capture_line(&c, None)));
            }
            v
        };
        let d = Discovery::run(|c| read(c, &mut lines, &mut rows)).map_err(|e| anyhow::anyhow!("{e}"))?;
        read(read_current_profile(), &mut lines, &mut rows);
        for &s in &d.sensors {
            read(read_temperature(s), &mut lines, &mut rows);
        }
        for &f in &d.fans {
            for c in [read_fan_rpm(f), read_fan_min_rpm(f), read_fan_max_rpm(f), read_fan_boost(f)] {
                read(c, &mut lines, &mut rows);
            }
        }
        let cmp = compare(&parse_capture(REFERENCE_CAPTURE), &rows);
        let failed: Vec<_> = cmp.iter().filter(|c| !c.ok).collect();
        let mut s = String::new();
        let _ = writeln!(s, "- Discovery: {} fans {:02X?}, {} sensors {:02X?}, profiles {:02X?}", d.fans.len(), d.fans, d.sensors.len(), d.sensors, d.profiles);
        let _ = writeln!(s, "- Fan → sensor: {:02X?}", d.fan_sensor);
        let profile = rows.iter().find(|r| r.1 == read_current_profile().packed()).map(|r| r.2 as u8);
        let _ = writeln!(s, "- Current profile: {}", profile.map(|p| format!("0x{p:02X} {}", profile_name(p))).unwrap_or_else(|| "unread".into()));
        let _ = writeln!(s, "- Compared with 2026-10-07: {} of {} calls match ({} failed)", cmp.len() - failed.len(), cmp.len(), failed.len());
        for c in &failed {
            let _ = writeln!(s, "  - {} 0x{:08X} [{}]: then 0x{:08X}, now {}", c.method, c.packed, c.rule, c.reference, c.now.map(|v| format!("0x{v:08X}")).unwrap_or_else(|| "no answer".into()));
        }
        for e in &errors {
            let _ = writeln!(s, "  - error: {e}");
        }
        write(out, "awcc.txt", &(lines.join("\n") + "\n"))?;
        write(out, "awcc-compare.json", &serde_json::to_string_pretty(&cmp)?)?;
        write(out, "awcc-discovery.json", &serde_json::to_string_pretty(&d)?)?;
        Ok(s)
    }

    fn watch(out: &Path, secs: u64, every: u64) -> Result<String> {
        let fw = wmi::Awcc::open()?;
        let d = Discovery::run(|c| fw.read(c).ok()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut csv = String::from("ms");
        for s in &d.sensors {
            let _ = write!(csv, ",sensor_0x{s:02X}_C");
        }
        for f in &d.fans {
            let _ = write!(csv, ",fan_0x{f:02X}_rpm");
        }
        csv.push_str(",profile\n");
        let start = Instant::now();
        let mut samples = Vec::new();
        while start.elapsed() < Duration::from_secs(secs) {
            let t_ms = start.elapsed().as_millis() as u64;
            let temps: BTreeMap<u8, i32> = d.sensors.iter().filter_map(|&s| fw.read(read_temperature(s)).ok().map(|v| (s, v as i32))).collect();
            let rpm: BTreeMap<u8, u32> = d.fans.iter().filter_map(|&f| fw.read(read_fan_rpm(f)).ok().map(|v| (f, v))).collect();
            let profile = fw.read(read_current_profile()).ok();
            let _ = write!(csv, "{t_ms}");
            for s in &d.sensors {
                let _ = write!(csv, ",{}", temps.get(s).map(|v| v.to_string()).unwrap_or_default());
            }
            for f in &d.fans {
                let _ = write!(csv, ",{}", rpm.get(f).map(|v| v.to_string()).unwrap_or_default());
            }
            let _ = writeln!(csv, ",{}", profile.map(|p| format!("0x{:02X}", p as u8)).unwrap_or_default());
            samples.push(Sample { t_ms, temps, rpm });
            std::thread::sleep(Duration::from_secs(every));
        }
        write(out, "fans.csv", &csv)?;
        let faults = FanPairWatch::scan(&d, &samples);
        let mut s = String::new();
        let _ = writeln!(s, "- {} samples over {} s (fans.csv)", samples.len(), secs);
        for f in &d.fans {
            let vals: Vec<u32> = samples.iter().filter_map(|x| x.rpm.get(f).copied()).collect();
            let (lo, hi) = (vals.iter().min().copied().unwrap_or(0), vals.iter().max().copied().unwrap_or(0));
            let zero = vals.iter().filter(|v| **v == 0).count();
            let _ = writeln!(s, "- Fan 0x{f:02X} (sensor 0x{:02X}): {lo}–{hi} RPM, 0 RPM in {zero} of {} samples", d.fan_sensor.get(f).copied().unwrap_or(0), vals.len());
        }
        for sid in &d.sensors {
            let vals: Vec<i32> = samples.iter().filter_map(|x| x.temps.get(sid).copied()).collect();
            let _ = writeln!(s, "- Sensor 0x{sid:02X}: {}–{} °C", vals.iter().min().copied().unwrap_or(0), vals.iter().max().copied().unwrap_or(0));
        }
        if faults.is_empty() {
            let _ = writeln!(s, "- Fan-pair check: no fault (needs the sensor above 80 °C for 2 min to be able to fire)");
        }
        for f in faults {
            let _ = writeln!(s, "- **Likely fan fault: fan 0x{:02X}** stood still for {} s while sensor 0x{:02X} reached {} °C and its partner spun. Check it with HWiNFO, then service it before any fan-control testing.", f.fan, (f.to_ms - f.from_ms) / 1000, f.sensor, f.peak_c);
        }
        Ok(s)
    }

    fn keys_cmd(out: &Path, secs: u64) -> Result<String> {
        let poll: Option<Box<dyn FnMut() -> Option<String>>> = if is_admin() {
            wmi::Awcc::open().ok().map(|fw| Box::new(move || fw.read(read_current_profile()).ok().map(|p| format!("0x{:02X} {}", p as u8, profile_name(p as u8)))) as Box<dyn FnMut() -> Option<String>>)
        } else {
            None
        };
        let polled = poll.is_some();
        let cap = keys::capture(secs, poll)?;
        write(out, "keys.json", &serde_json::to_string_pretty(&cap)?)?;
        let mut s = String::new();
        let _ = writeln!(s, "- Devices listened to: {}", cap.devices.len());
        for d in &cap.devices {
            let _ = writeln!(s, "  - {} page 0x{:04X} usage 0x{:04X} {}", d.kind, d.usage_page, d.usage, d.name);
        }
        let (mut hid, mut key) = (0, 0);
        let mut changes = Vec::new();
        let mut last: Option<String> = None;
        for e in &cap.events {
            match e {
                keys::Event::Hid { .. } => hid += 1,
                keys::Event::Key { .. } => key += 1,
                keys::Event::Profile { ms, profile } => {
                    if profile.is_some() && *profile != last {
                        if last.is_some() {
                            changes.push(format!("{:.1} s → {}", *ms as f64 / 1000.0, profile.clone().unwrap_or_default()));
                        }
                        last = profile.clone();
                    }
                }
            }
        }
        let _ = writeln!(s, "- Recorded: {hid} HID reports, {key} special-key events; {} typing keys dropped unrecorded", cap.typing_keys_dropped);
        if polled {
            let _ = writeln!(s, "- Profile changes while listening: {}", if changes.is_empty() { "none".into() } else { changes.join(", ") });
        } else {
            let _ = writeln!(s, "- Profile not polled (not elevated)");
        }
        Ok(s)
    }

    fn power_cmd(out: &Path) -> Result<String> {
        let rows = power::sample(Duration::from_secs(1))?;
        let mut s = String::new();
        let mut json = Vec::new();
        for (path, inst, v) in &rows {
            // Energy Meter reports milliwatts.
            let shown = match v {
                Ok(x) if path.contains("Energy Meter") => format!("{:.1} W", x / 1000.0),
                Ok(x) => format!("{x:.1}"),
                Err(e) => e.clone(),
            };
            let _ = writeln!(s, "- {path}{}: {shown}", if inst.is_empty() { String::new() } else { format!(" [{inst}]") });
            json.push(serde_json::json!({ "counter": path, "instance": inst, "value": v.as_ref().ok(), "error": v.as_ref().err() }));
        }
        write(out, "power.json", &serde_json::to_string_pretty(&json)?)?;
        Ok(s)
    }
}
