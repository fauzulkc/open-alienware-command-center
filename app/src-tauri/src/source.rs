//! Where readings come from, in order: the hardware service (normal), a direct firmware read when this app
//! runs elevated, or the 2026-10-07 capture when the person turns on demo data. Never a guess: with none of
//! these the app says so and shows the Windows-side numbers (CPU power, RAM, GPU, network, processes) only.

use crate::processes::{gpu_pid, Sampler, Top};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Service,
    Direct,
    Demo,
    Unavailable,
}

/// One moment of the charts, with what was using the machine then.
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub t: u64,
    /// Wall-clock time, ms since the Unix epoch (for the hover read-out).
    pub at: u64,
    pub cpu_c: Option<i64>,
    pub gpu_c: Option<i64>,
    pub cpu_w: Option<f64>,
    pub fans: Vec<Option<u64>>,
    pub profile: Option<String>,
    pub ram_gb: f64,
    /// Total CPU load, % of all cores.
    pub cpu_pct: f64,
    pub gpu_pct: Option<f64>,
    pub net_kbps: Option<f64>,
    pub top: Top,
}

#[derive(Default)]
pub struct State {
    pub source: Option<Source>,
    pub snapshot: Option<Value>,
    pub lists: Option<Value>,
    pub power: Option<Value>,
    pub error: Option<String>,
    pub history: VecDeque<Point>,
    pub announced: Vec<String>,
    pub updated: Option<Instant>,
}

pub const HISTORY: usize = 600;

pub type Shared = Arc<Mutex<State>>;

const COUNTERS: &[&str] = &["\\Energy Meter(*)\\Power", "\\Processor Information(_Total)\\% Processor Performance", "\\GPU Engine(*)\\Utilization Percentage", "\\Network Interface(*)\\Bytes Total/sec"];

struct Windows {
    power: Value,
    gpu_by_pid: HashMap<u32, f64>,
    gpu_pct: Option<f64>,
    net_kbps: Option<f64>,
}

/// One sample of the Windows counters: CPU package power, GPU engines (per process), network total.
fn windows_counters() -> Windows {
    let rows = wiregent_hw::power::sample_paths(COUNTERS, Duration::from_millis(400)).unwrap_or_default();
    let mw = |inst: &str| rows.iter().find(|(p, i, _)| p.contains("Energy Meter") && i == inst).and_then(|(_, _, v)| v.as_ref().ok().map(|mw| (mw / 100.0).round() / 10.0));
    let perf = rows.iter().find(|(p, _, _)| p.contains("% Processor Performance")).and_then(|(_, _, v)| v.as_ref().ok().copied());
    let mut gpu_by_pid: HashMap<u32, f64> = HashMap::new();
    let mut per_engine: HashMap<String, f64> = HashMap::new();
    for (p, inst, v) in &rows {
        if !p.contains("GPU Engine") {
            continue;
        }
        let Ok(v) = v else { continue };
        if let Some(pid) = gpu_pid(inst) {
            let e = gpu_by_pid.entry(pid).or_default();
            *e = e.max(*v);
        }
        let engine = inst.split("_engtype_").nth(1).unwrap_or("").to_string();
        *per_engine.entry(engine).or_default() += v;
    }
    let gpu_pct = per_engine.values().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))).map(|v| v.min(100.0).round());
    let net: Vec<f64> = rows.iter().filter(|(p, _, _)| p.contains("Network Interface")).filter_map(|(_, _, v)| v.as_ref().ok().copied()).collect();
    let net_kbps = (!net.is_empty()).then(|| (net.iter().sum::<f64>() / 1024.0).round());
    Windows { power: json!({ "cpuPackageW": mw("RAPL_Package0_PKG"), "cpuCoresW": mw("RAPL_Package0_PP0"), "cpuPerformancePct": perf }), gpu_by_pid, gpu_pct, net_kbps }
}

fn sensor(snapshot: &Value, name: &str) -> Option<i64> {
    snapshot["sensors"].as_array()?.iter().find(|s| s["name"] == name).and_then(|s| s["celsius"].as_i64())
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runs forever on its own thread. `demo` is read each tick (the person can switch it on and off).
pub fn run(state: Shared, demo: Arc<Mutex<bool>>, on_alert: impl Fn(String) + Send + 'static) {
    use wiregent_hw::reader::Reader;
    let start = Instant::now();
    let mut direct: Option<Reader<Box<dyn wiregent_hw::Firmware>>> = None;
    let mut demo_reader: Option<Reader<Box<dyn wiregent_hw::Firmware>>> = None;
    let mut sampler = Sampler::new();
    let mut last_tick = Instant::now();
    loop {
        let t = start.elapsed().as_millis() as u64;
        let (mut source, mut snapshot, mut lists, mut error) = (Source::Unavailable, None, None, None);
        if wiregent_hw::client::service_present() {
            use wiregent_hw::proto::{Request, Response, PIPE_NAME};
            match wiregent_hw::client::request(PIPE_NAME, &Request::Snapshot, wiregent_hw::client::SERVICE_IMAGE) {
                Ok(Response::Snapshot { snapshot: s, .. }) => {
                    source = Source::Service;
                    snapshot = s;
                    if let Ok(Response::Lists { lists: l }) = wiregent_hw::client::request(PIPE_NAME, &Request::Lists, wiregent_hw::client::SERVICE_IMAGE) {
                        lists = Some(l);
                    }
                }
                Ok(other) => error = Some(format!("the hardware service answered {other:?}")),
                Err(e) => error = Some(format!("{e:#}")),
            }
        }
        if source == Source::Unavailable && wiregent_hw::is_admin() {
            if direct.is_none() {
                direct = wiregent_hw::wmi::Awcc::open().and_then(|fw| Reader::new(Box::new(fw) as Box<dyn wiregent_hw::Firmware>)).map_err(|e| error = Some(format!("{e:#}"))).ok();
            }
            if let Some(r) = direct.as_mut() {
                source = Source::Direct;
                snapshot = serde_json::to_value(r.tick(t)).ok();
                lists = serde_json::to_value(r.lists()).ok();
            }
        }
        if source == Source::Unavailable && *demo.lock().unwrap() {
            let r = demo_reader.get_or_insert_with(|| Reader::new(Box::new(wiregent_hw::replay::Replay::october()) as Box<dyn wiregent_hw::Firmware>).expect("the October capture decodes"));
            source = Source::Demo;
            snapshot = serde_json::to_value(r.tick(t)).ok();
            lists = serde_json::to_value(r.lists()).ok();
        }
        let win = windows_counters();
        let (top, ram_gb, cpu_pct) = sampler.sample(&win.gpu_by_pid, last_tick.elapsed().as_secs_f64());
        last_tick = Instant::now();
        let mut alerts = Vec::new();
        {
            let mut st = state.lock().unwrap();
            let s = snapshot.clone().unwrap_or(Value::Null);
            st.history.push_back(Point {
                t,
                at: now_epoch_ms(),
                cpu_c: sensor(&s, "CPU"),
                gpu_c: sensor(&s, "GPU"),
                cpu_w: win.power["cpuPackageW"].as_f64(),
                fans: s["fans"].as_array().map(|f| f.iter().map(|x| x["rpm"].as_u64()).collect()).unwrap_or_default(),
                profile: s["profile_name"].as_str().map(String::from),
                ram_gb,
                cpu_pct,
                gpu_pct: win.gpu_pct,
                net_kbps: win.net_kbps,
                top,
            });
            while st.history.len() > HISTORY {
                st.history.pop_front();
            }
            for a in s["alerts"].as_array().into_iter().flatten() {
                let text = alert_text(a);
                if source != Source::Demo && !st.announced.contains(&text) {
                    st.announced.push(text.clone());
                    alerts.push(text);
                }
            }
            st.source = Some(source);
            st.snapshot = snapshot;
            if lists.is_some() {
                st.lists = lists;
            }
            st.power = Some(win.power);
            st.error = error;
            st.updated = Some(Instant::now());
        }
        for a in alerts {
            on_alert(a);
        }
        let hot = state.lock().unwrap().history.back().and_then(|p| p.cpu_c).unwrap_or(0) >= 80;
        std::thread::sleep(Duration::from_millis(if hot { 700 } else { 1_600 }));
    }
}

/// The person-facing text of a reader alert.
pub fn alert_text(a: &Value) -> String {
    match a["kind"].as_str() {
        Some("fan-fault") => format!("Fan 0x{:02X} stood still while its sensor was hot and its partner spun: likely a fan fault.", a["fan"].as_u64().unwrap_or(0)),
        Some("stuck-reading") => format!("Fan 0x{:02X} reports a speed that never changes ({} RPM): its reading isn't a real measurement.", a["fan"].as_u64().unwrap_or(0), a["rpm"].as_u64().unwrap_or(0)),
        Some("sensor-stale") => format!("No valid {} temperature for 30 s or more.", a["name"].as_str().unwrap_or("sensor")),
        _ => a.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_texts_match_the_reader_alerts() {
        assert!(alert_text(&json!({ "kind": "fan-fault", "fan": 59 })).contains("0x3B"));
        assert!(alert_text(&json!({ "kind": "stuck-reading", "fan": 60, "rpm": 3000 })).contains("3000 RPM"));
        assert!(alert_text(&json!({ "kind": "sensor-stale", "name": "CPU" })).contains("CPU"));
    }
}
