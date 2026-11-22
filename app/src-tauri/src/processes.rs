//! What was using the machine at each moment of the charts: the top processes by CPU, RAM, GPU and disk,
//! grouped by program name (so a browser's twenty processes count as one) and sorted highest first.
//! Per-process network needs admin-level tracing on Windows, so network is a machine-wide total here.

use serde::Serialize;
use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Proc {
    pub name: String,
    pub value: f64,
    /// How many processes of this program were added up.
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Top {
    /// % of the whole CPU (all cores = 100).
    pub cpu: Vec<Proc>,
    /// MB of private memory.
    pub ram: Vec<Proc>,
    /// % of the busiest GPU engine.
    pub gpu: Vec<Proc>,
    /// KB/s read + written.
    pub disk: Vec<Proc>,
}

pub const KEEP: usize = 6;

/// Adds up per-process values by program name, drops near-zero ones, keeps the highest `KEEP`.
pub fn rank(items: impl IntoIterator<Item = (String, f64)>, floor: f64) -> Vec<Proc> {
    let mut by: HashMap<String, (f64, u32)> = HashMap::new();
    for (name, v) in items {
        let e = by.entry(name).or_default();
        e.0 += v;
        e.1 += 1;
    }
    let mut out: Vec<Proc> = by.into_iter().filter(|(_, (v, _))| *v >= floor).map(|(name, (value, count))| Proc { name, value: (value * 10.0).round() / 10.0, count }).collect();
    out.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.name.cmp(&b.name)));
    out.truncate(KEEP);
    out
}

/// `pid_1234_luid_0x…_phys_0_eng_3_engtype_3D` → 1234
pub fn gpu_pid(instance: &str) -> Option<u32> {
    instance.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

pub struct Sampler {
    sys: System,
    cores: f64,
    interval_s: f64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get() as f64).unwrap_or(1.0);
        Sampler { sys: System::new(), cores, interval_s: 2.0 }
    }

    /// `gpu_by_pid`: % per process from the GPU-engine counters. `interval_s`: time since the last sample.
    /// Returns the top programs, RAM in use (GB) and total CPU load (% of all cores).
    pub fn sample(&mut self, gpu_by_pid: &HashMap<u32, f64>, interval_s: f64) -> (Top, f64, f64) {
        self.interval_s = interval_s.max(0.2);
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new().with_cpu().with_memory().with_disk_usage());
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        let name = |p: &sysinfo::Process| p.name().to_string_lossy().trim_end_matches(".exe").to_string();
        let procs: Vec<_> = self.sys.processes().iter().filter(|(pid, _)| pid.as_u32() != 0).collect();
        let cpu = rank(procs.iter().map(|(_, p)| (name(p), p.cpu_usage() as f64 / self.cores)), 0.2);
        let ram = rank(procs.iter().map(|(_, p)| (name(p), p.memory() as f64 / 1_048_576.0)), 20.0);
        let disk = rank(procs.iter().map(|(_, p)| { let d = p.disk_usage(); (name(p), (d.read_bytes + d.written_bytes) as f64 / 1024.0 / self.interval_s) }), 50.0);
        let names: HashMap<u32, String> = procs.iter().map(|(pid, p)| (pid.as_u32(), name(p))).collect();
        let gpu = rank(gpu_by_pid.iter().filter_map(|(pid, v)| names.get(pid).map(|n| (n.clone(), *v))), 0.5);
        let used_gb = (self.sys.used_memory() as f64 / 1_073_741_824.0 * 10.0).round() / 10.0;
        let cpu_pct = (self.sys.global_cpu_usage() as f64).round();
        (Top { cpu, ram, gpu, disk }, used_gb, cpu_pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_added_up_sorted_and_trimmed() {
        let r = rank([("chrome".into(), 10.0), ("chrome".into(), 5.0), ("claude".into(), 12.0), ("idle".into(), 0.1), ("a".into(), 1.0), ("b".into(), 2.0), ("c".into(), 3.0), ("d".into(), 4.0), ("e".into(), 4.0)], 0.2);
        assert_eq!(r.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["chrome", "claude", "d", "e", "c", "b"]);
        assert_eq!((r[0].value, r[0].count), (15.0, 2));
        assert!(r.iter().all(|p| p.name != "idle"), "below the floor");
    }

    #[test]
    fn gpu_counter_instances_name_their_process() {
        assert_eq!(gpu_pid("pid_4242_luid_0x00000000_0x0000D1E7_phys_0_eng_0_engtype_3D"), Some(4242));
        assert_eq!(gpu_pid("_Total"), None);
    }
}
