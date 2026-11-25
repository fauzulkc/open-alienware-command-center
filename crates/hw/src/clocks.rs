//! Live CPU and GPU clocks, like CPU-Z and GPU-Z, without a driver.
//!
//! - CPU: each physical core's running clock = its rated (base) clock x Windows' `% Processor Performance`
//!   for its threads (`\Processor Information(*)`, which Windows measures from the CPU's own APERF/MPERF
//!   counters, so turbo is in it and it can pass 100 %). The rated clock comes from the same counters' `Processor Frequency` (2300 MHz on the
//!   owner's P-cores, 1700 on the E-cores; an idle thread sometimes dips to ~1500, so each kind of core takes
//!   the highest value any of its threads has shown). `CallNtPowerInformation`'s `MaxMhz` isn't used: it
//!   reports 2300 for the E-cores too. Which threads make a core, and whether it's a performance or an
//!   efficient core, comes from `GetLogicalProcessorInformationEx(RelationProcessorCore)` (`EfficiencyClass`:
//!   the highest class is the P-cores). It's an average over the time since the last
//!   read, as Task Manager's "Speed" is.
//! - GPU: NVIDIA's own management library (`nvml.dll`, installed with the driver, loaded from System32 only):
//!   the graphics and memory clocks, their maximums, the performance state, power draw, load and
//!   temperature. Reading it can wake a sleeping NVIDIA GPU on a laptop, so the app reads clocks only while a
//!   page that shows them is open. Intel's integrated graphics has no public interface for its clock.
//!
//! - Power ([`PowerDraw`]): the CPU's own energy meters (`\Energy Meter(*)\Power`: the package, the cores,
//!   the built-in graphics, memory where the CPU measures it), the NVIDIA GPU's power from its driver, and the
//!   battery's charge or discharge rate (`root\wmi BatteryStatus`). On battery the discharge rate is the whole
//!   laptop's draw, so "the rest" (screen, memory, SSD, fans, Wi-Fi...) is that minus the measured parts. On AC
//!   Windows doesn't report what the adapter delivers, so the input isn't known there.
//!
//! [`cores_from`] and [`power_from`] are the pure parts (tested here); [`win::ClockReader`] reads the real
//! values (Windows).

use serde::Serialize;

/// One physical core's clock.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreClock {
    /// "P0".."P5", "E0".."E7" on a hybrid CPU, "C0".. otherwise.
    pub label: String,
    /// An efficient (E) core.
    pub efficient: bool,
    /// Its logical processors (threads).
    pub threads: Vec<u32>,
    /// Its rated clock, MHz.
    pub base_mhz: u32,
    /// Its running clock now, MHz (the average of its threads').
    pub mhz: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuClocks {
    pub cores: Vec<CoreClock>,
    /// The average over every core, MHz.
    pub avg_mhz: f64,
    /// The fastest core now, MHz.
    pub max_mhz: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuClocks {
    pub name: String,
    /// The graphics (core) clock now, MHz.
    pub core_mhz: Option<u32>,
    /// The memory clock now, MHz (as the driver and nvidia-smi report it).
    pub mem_mhz: Option<u32>,
    pub max_core_mhz: Option<u32>,
    pub max_mem_mhz: Option<u32>,
    /// The performance state: 0 (P0, full speed) to 15 (lowest).
    pub pstate: Option<u32>,
    pub power_w: Option<f64>,
    /// Load, %.
    pub load_pct: Option<u32>,
    pub temp_c: Option<u32>,
    /// The power limit the driver enforces now, W.
    pub power_limit_w: Option<f64>,
}

/// The battery now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatteryNow {
    pub on_ac: bool,
    /// Power going into the battery, W (0 when not charging).
    pub charging_w: f64,
    /// Power coming out of the battery, W (0 on AC).
    pub discharging_w: f64,
}

/// Where the power goes, W (measured parts only; `None` where nothing reports it).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerDraw {
    /// The whole CPU package (cores, built-in graphics and the rest of the chip).
    pub cpu_package_w: Option<f64>,
    pub cpu_cores_w: Option<f64>,
    pub igpu_w: Option<f64>,
    /// Memory, where the CPU measures it (often not: then `None`).
    pub dram_w: Option<f64>,
    /// The package less the cores and the built-in graphics: caches, memory controller, I/O.
    pub cpu_rest_w: Option<f64>,
    pub gpu_w: Option<f64>,
    pub gpu_limit_w: Option<f64>,
    /// Every measured part: the CPU package, memory and the NVIDIA GPU.
    pub measured_w: f64,
    pub battery: Option<BatteryNow>,
    /// Power coming in: on battery its discharge rate (the whole laptop); on AC unknown.
    pub input_w: Option<f64>,
    /// Power going out: the laptop's draw plus what charges the battery. Known on battery (= input).
    pub output_w: Option<f64>,
    /// The parts nothing measures (screen, memory, SSD, fans, Wi-Fi...): input less the measured parts, on battery.
    pub rest_w: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clocks {
    pub cpu: Option<CpuClocks>,
    pub gpu: Option<GpuClocks>,
    pub power: Option<PowerDraw>,
    /// Why there is no CPU or GPU reading, in words.
    pub cpu_error: Option<String>,
    pub gpu_error: Option<String>,
}

/// The cores from the topology (each core's efficiency class and threads), each thread's rated clock (MHz)
/// and each thread's performance % now: P-cores first, then E-cores, in the order Windows numbers them. A
/// core's clock is its threads' average; a thread without a reading counts at its rated clock x 0 (idle
/// parked threads read 0 % too).
pub fn cores_from(topology: &[(u8, Vec<u32>)], base: &[(u32, u32)], perf: &[(u32, f64)]) -> CpuClocks {
    let top = topology.iter().map(|(c, _)| *c).max().unwrap_or(0);
    let hybrid = topology.iter().any(|(c, _)| *c != top);
    let mut cores: Vec<CoreClock> = topology
        .iter()
        .map(|(class, threads)| {
            let rated = |t: &u32| base.iter().find(|(n, _)| n == t).map_or(0, |(_, m)| *m);
            let base_mhz = threads.iter().map(rated).max().unwrap_or(0);
            let speeds: Vec<f64> = threads.iter().map(|t| rated(t) as f64 * perf.iter().find(|(n, _)| n == t).map_or(0.0, |(_, p)| *p) / 100.0).collect();
            let mhz = if speeds.is_empty() { 0.0 } else { speeds.iter().sum::<f64>() / speeds.len() as f64 };
            CoreClock { label: String::new(), efficient: hybrid && *class != top, threads: threads.clone(), base_mhz, mhz }
        })
        .collect();
    // P-cores first, keeping Windows' order within each kind.
    cores.sort_by_key(|c| c.efficient);
    let (mut p, mut e) = (0, 0);
    for c in cores.iter_mut() {
        c.label = if !hybrid {
            p += 1;
            format!("C{}", p - 1)
        } else if c.efficient {
            e += 1;
            format!("E{}", e - 1)
        } else {
            p += 1;
            format!("P{}", p - 1)
        };
    }
    let avg = if cores.is_empty() { 0.0 } else { cores.iter().map(|c| c.mhz).sum::<f64>() / cores.len() as f64 };
    let max = cores.iter().map(|c| c.mhz).fold(0.0, f64::max);
    CpuClocks { cores, avg_mhz: avg, max_mhz: max }
}

/// Each thread's rated clock from the threads' `Processor Frequency` readings: the highest reading among the
/// threads of the same efficiency class, folded into `seen` (class -> highest so far) so a dip on an idle
/// thread never lowers it.
pub fn rated_from(topology: &[(u8, Vec<u32>)], freq: &[(u32, f64)], seen: &mut Vec<(u8, u32)>) -> Vec<(u32, u32)> {
    for (class, threads) in topology {
        let now = threads.iter().filter_map(|t| freq.iter().find(|(n, _)| n == t).map(|(_, f)| f.round() as u32)).max().unwrap_or(0);
        match seen.iter_mut().find(|(c, _)| c == class) {
            Some(slot) => slot.1 = slot.1.max(now),
            None => seen.push((*class, now)),
        }
    }
    topology.iter().flat_map(|(class, threads)| {
        let r = seen.iter().find(|(c, _)| c == class).map_or(0, |(_, m)| *m);
        threads.iter().map(move |t| (*t, r))
    }).collect()
}

/// The power picture from the CPU's energy meter rails (instance name lower-cased -> W), the GPU and the
/// battery. A rail reading 0 counts as not measured (the CPU reports unsupported rails as 0).
pub fn power_from(rails: &[(String, f64)], gpu: Option<&GpuClocks>, battery: Option<BatteryNow>) -> PowerDraw {
    let rail = |suffix: &str| rails.iter().find(|(n, _)| n.ends_with(suffix)).map(|(_, w)| *w).filter(|w| *w > 0.0);
    let pkg = rail("_pkg");
    let cores = rail("_pp0");
    let igpu = rail("_pp1");
    let dram = rail("_dram");
    let cpu_rest = pkg.map(|p| (p - cores.unwrap_or(0.0) - igpu.unwrap_or(0.0)).max(0.0));
    let gpu_w = gpu.and_then(|g| g.power_w);
    let measured = pkg.unwrap_or(0.0) + dram.unwrap_or(0.0) + gpu_w.unwrap_or(0.0);
    let (input, output, rest) = match battery {
        Some(b) if !b.on_ac && b.discharging_w > 0.0 => (Some(b.discharging_w), Some(b.discharging_w), Some((b.discharging_w - measured).max(0.0))),
        _ => (None, None, None),
    };
    PowerDraw { cpu_package_w: pkg, cpu_cores_w: cores, igpu_w: igpu, dram_w: dram, cpu_rest_w: cpu_rest, gpu_w, gpu_limit_w: gpu.and_then(|g| g.power_limit_w), measured_w: measured, battery, input_w: input, output_w: output, rest_w: rest }
}

/// The logical processor number in a `\Processor Information` instance name ("0,13" -> 13), `None` for the
/// totals ("_Total", "0,_Total") and other processor groups than 0.
pub fn thread_of(instance: &str) -> Option<u32> {
    let (group, n) = instance.split_once(',')?;
    if group != "0" {
        return None;
    }
    n.parse().ok()
}

#[cfg(windows)]
pub mod win {
    use super::{cores_from, power_from, rated_from, thread_of, BatteryNow, Clocks, GpuClocks};
    use std::ffi::c_void;
    use windows::core::{w, PCSTR, PCWSTR};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
    use windows::Win32::System::Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY};
    use windows::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, LOGICAL_PROCESSOR_RELATIONSHIP};

    const PDH_MORE_DATA: u32 = 0x8000_07D2;
    /// The shortest time between two collects that gives a sound average.
    const MIN_GAP: std::time::Duration = std::time::Duration::from_millis(300);
    /// `RelationProcessorCore`.
    const RELATION_PROCESSOR_CORE: LOGICAL_PROCESSOR_RELATIONSHIP = LOGICAL_PROCESSOR_RELATIONSHIP(0);

    /// Each physical core's efficiency class and threads (processor group 0).
    fn topology() -> Result<Vec<(u8, Vec<u32>)>, String> {
        let mut len = 0u32;
        // SAFETY: a size query (no buffer), then a buffer of the size asked for; records are read within it.
        unsafe {
            let _ = GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, None, &mut len);
            if len == 0 {
                return Err("GetLogicalProcessorInformationEx gave no size".into());
            }
            let mut buf = vec![0u8; len as usize];
            GetLogicalProcessorInformationEx(RELATION_PROCESSOR_CORE, Some(buf.as_mut_ptr().cast()), &mut len).map_err(|e| format!("GetLogicalProcessorInformationEx: {e}"))?;
            let mut out = Vec::new();
            let mut off = 0usize;
            while off + 48 <= len as usize {
                let size = u32::from_ne_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
                if size == 0 {
                    break;
                }
                // PROCESSOR_RELATIONSHIP at +8: Flags u8, EfficiencyClass u8, Reserved[20], GroupCount u16 (+30),
                // then GROUP_AFFINITY[GroupCount] at +32: Mask usize, Group u16, Reserved[3] u16 (16 bytes).
                let class = buf[off + 9];
                let groups = u16::from_ne_bytes(buf[off + 30..off + 32].try_into().unwrap()) as usize;
                let mut threads = Vec::new();
                for g in 0..groups {
                    let a = off + 32 + g * 16;
                    if a + 10 > buf.len() {
                        break;
                    }
                    let mask = u64::from_ne_bytes(buf[a..a + 8].try_into().unwrap());
                    let group = u16::from_ne_bytes(buf[a + 8..a + 10].try_into().unwrap());
                    if group == 0 {
                        threads.extend((0..64).filter(|b| mask & (1u64 << b) != 0));
                    }
                }
                if !threads.is_empty() {
                    out.push((class, threads));
                }
                off += size;
            }
            Ok(out)
        }
    }

    type Fn0 = unsafe extern "C" fn() -> i32;
    type FnCount = unsafe extern "C" fn(*mut u32) -> i32;
    type FnHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
    type FnName = unsafe extern "C" fn(*mut c_void, *mut u8, u32) -> i32;
    type FnClock = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
    type FnU32 = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
    type FnTemp = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
    #[repr(C)]
    #[derive(Default)]
    struct Utilization {
        gpu: u32,
        memory: u32,
    }
    type FnUtil = unsafe extern "C" fn(*mut c_void, *mut Utilization) -> i32;

    /// NVIDIA's management library, loaded once.
    struct Nvml {
        device: *mut c_void,
        name: String,
        clock: FnClock,
        max_clock: FnClock,
        pstate: FnU32,
        power: FnU32,
        util: FnUtil,
        temp: FnTemp,
        limit: FnU32,
        shutdown: Fn0,
    }

    const NVML_CLOCK_GRAPHICS: u32 = 0;
    const NVML_CLOCK_MEM: u32 = 2;
    const NVML_TEMPERATURE_GPU: u32 = 0;

    impl Nvml {
        fn open() -> Result<Nvml, String> {
            // SAFETY: nvml.dll from System32 only (no search of the current or app folder); every function is
            // looked up by its documented name and called with the documented C signature. The library is
            // shut down once: on a failure here, or when the `Nvml` built at the end is dropped.
            unsafe {
                let lib: HMODULE = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).map_err(|_| "no NVIDIA driver library (nvml.dll)".to_string())?;
                let get = |name: &[u8]| GetProcAddress(lib, PCSTR(name.as_ptr())).ok_or_else(|| format!("nvml.dll has no {}", String::from_utf8_lossy(&name[..name.len() - 1])));
                let init: Fn0 = std::mem::transmute(get(b"nvmlInit_v2\0")?);
                let shutdown: Fn0 = std::mem::transmute(get(b"nvmlShutdown\0")?);
                let count: FnCount = std::mem::transmute(get(b"nvmlDeviceGetCount_v2\0")?);
                let handle: FnHandle = std::mem::transmute(get(b"nvmlDeviceGetHandleByIndex_v2\0")?);
                let name_fn: FnName = std::mem::transmute(get(b"nvmlDeviceGetName\0")?);
                let clock: FnClock = std::mem::transmute(get(b"nvmlDeviceGetClockInfo\0")?);
                let max_clock: FnClock = std::mem::transmute(get(b"nvmlDeviceGetMaxClockInfo\0")?);
                let pstate: FnU32 = std::mem::transmute(get(b"nvmlDeviceGetPerformanceState\0")?);
                let power: FnU32 = std::mem::transmute(get(b"nvmlDeviceGetPowerUsage\0")?);
                let util: FnUtil = std::mem::transmute(get(b"nvmlDeviceGetUtilizationRates\0")?);
                let temp: FnTemp = std::mem::transmute(get(b"nvmlDeviceGetTemperature\0")?);
                let limit: FnU32 = std::mem::transmute(get(b"nvmlDeviceGetEnforcedPowerLimit\0")?);
                let st = init();
                if st != 0 {
                    return Err(format!("the NVIDIA library didn't start (NVML {st})"));
                }
                let mut c = 0u32;
                if count(&mut c) != 0 || c == 0 {
                    shutdown();
                    return Err("no NVIDIA GPU found".into());
                }
                let mut device = std::ptr::null_mut();
                if handle(0, &mut device) != 0 {
                    shutdown();
                    return Err("the NVIDIA GPU can't be opened".into());
                }
                let mut buf = [0u8; 96];
                let name = if name_fn(device, buf.as_mut_ptr(), buf.len() as u32) == 0 { String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == 0).unwrap_or(0)]).into_owned() } else { "NVIDIA GPU".into() };
                Ok(Nvml { device, name, clock, max_clock, pstate, power, util, temp, limit, shutdown })
            }
        }

        fn read(&self) -> GpuClocks {
            let ok = |st: i32, v: u32| (st == 0).then_some(v);
            // SAFETY: the device handle from `open`, out-pointers to locals.
            unsafe {
                let (mut core, mut mem, mut max_core, mut max_mem, mut p, mut mw, mut t, mut lim) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
                let mut u = Utilization::default();
                let core = ok((self.clock)(self.device, NVML_CLOCK_GRAPHICS, &mut core), core);
                let mem = ok((self.clock)(self.device, NVML_CLOCK_MEM, &mut mem), mem);
                let max_core = ok((self.max_clock)(self.device, NVML_CLOCK_GRAPHICS, &mut max_core), max_core);
                let max_mem = ok((self.max_clock)(self.device, NVML_CLOCK_MEM, &mut max_mem), max_mem);
                let pstate = ok((self.pstate)(self.device, &mut p), p);
                let power_w = ok((self.power)(self.device, &mut mw), mw).map(|mw| mw as f64 / 1_000.0);
                let load = ((self.util)(self.device, &mut u) == 0).then_some(u.gpu);
                let temp = ok((self.temp)(self.device, NVML_TEMPERATURE_GPU, &mut t), t);
                let power_limit_w = ok((self.limit)(self.device, &mut lim), lim).map(|mw| mw as f64 / 1_000.0);
                GpuClocks { name: self.name.clone(), core_mhz: core, mem_mhz: mem, max_core_mhz: max_core, max_mem_mhz: max_mem, pstate, power_w, load_pct: load, temp_c: temp, power_limit_w }
            }
        }
    }

    impl Drop for Nvml {
        fn drop(&mut self) {
            // SAFETY: started in `open`.
            unsafe {
                (self.shutdown)();
            }
        }
    }

    /// Reads the clocks; keep one and call [`ClockReader::read`] about once a second (the CPU clocks are
    /// averages over the time between reads).
    pub struct ClockReader {
        query: PDH_HQUERY,
        perf: Option<PDH_HCOUNTER>,
        freq: Option<PDH_HCOUNTER>,
        energy: Option<PDH_HCOUNTER>,
        /// `root\wmi` for the battery, made on the first read (`None` inside: not available).
        battery: Option<Option<crate::wmi::Wmi>>,
        topology: Result<Vec<(u8, Vec<u32>)>, String>,
        /// The highest `Processor Frequency` seen per efficiency class.
        seen: Vec<(u8, u32)>,
        /// When the counters were last collected.
        last: std::time::Instant,
        nvml: Option<Result<Nvml, String>>,
    }

    // SAFETY: the PDH query and the NVML device handle are only used through `&mut self`, so from one thread at
    // a time (the app keeps the reader behind a mutex); neither API ties its handles to the creating thread.
    unsafe impl Send for ClockReader {}

    impl ClockReader {
        pub fn new() -> ClockReader {
            let mut query = PDH_HQUERY::default();
            let (mut perf, mut freq, mut energy) = (None, None, None);
            // SAFETY: PDH calls with locals; the query is closed in Drop.
            unsafe {
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) == 0 {
                    let mut c = PDH_HCOUNTER::default();
                    if PdhAddEnglishCounterW(query, w!("\\Processor Information(*)\\% Processor Performance"), 0, &mut c) == 0 {
                        perf = Some(c);
                    }
                    let mut f = PDH_HCOUNTER::default();
                    if PdhAddEnglishCounterW(query, w!("\\Processor Information(*)\\Processor Frequency"), 0, &mut f) == 0 {
                        freq = Some(f);
                    }
                    let mut e = PDH_HCOUNTER::default();
                    if PdhAddEnglishCounterW(query, w!("\\Energy Meter(*)\\Power"), 0, &mut e) == 0 {
                        energy = Some(e);
                    }
                    PdhCollectQueryData(query);
                }
            }
            ClockReader { query, perf, freq, energy, battery: None, topology: topology(), seen: Vec::new(), last: std::time::Instant::now(), nvml: None }
        }

        /// Each thread's value of `counter`, from the last collect.
        fn per_thread(&self, counter: Option<PDH_HCOUNTER>) -> Result<Vec<(u32, f64)>, String> {
            Ok(self.instances(counter)?.into_iter().filter_map(|(n, v)| thread_of(&n).map(|t| (t, v))).collect())
        }

        /// The battery now, from `root\wmi BatteryStatus` (rates in mW).
        fn battery_now(&mut self) -> Option<BatteryNow> {
            let w = self.battery.get_or_insert_with(|| crate::wmi::Wmi::connect("root\\wmi").ok()).as_ref()?;
            let rows = w.query("SELECT PowerOnline, ChargeRate, DischargeRate FROM BatteryStatus", &["PowerOnline", "ChargeRate", "DischargeRate"]).ok()?;
            let r = rows.first()?;
            let num = |s: &str| s.trim().parse::<f64>().unwrap_or(0.0) / 1_000.0;
            Some(BatteryNow { on_ac: r[0].eq_ignore_ascii_case("true"), charging_w: num(&r[1]), discharging_w: num(&r[2]) })
        }

        /// Every instance of `counter` (name, value), from the last collect.
        fn instances(&self, counter: Option<PDH_HCOUNTER>) -> Result<Vec<(String, f64)>, String> {
            let c = counter.ok_or("the CPU's performance counters aren't available")?;
            // SAFETY: the query and counter from `new`; the item buffer is sized as PDH asks and read within the
            // count it returns.
            unsafe {
                let (mut bytes, mut count) = (0u32, 0u32);
                let st = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut bytes, &mut count, None);
                if st != PDH_MORE_DATA {
                    return Err(format!("no CPU performance data yet (PDH 0x{st:08X})"));
                }
                let n = (bytes as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) + 1;
                let mut buf = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); n];
                let st = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut bytes, &mut count, Some(buf.as_mut_ptr()));
                if st != 0 {
                    return Err(format!("no CPU performance data (PDH 0x{st:08X})"));
                }
                Ok(buf
                    .iter()
                    .take(count as usize)
                    .filter_map(|it| {
                        let name = it.szName.to_string().ok()?;
                        (it.FmtValue.CStatus == 0).then(|| (name, it.FmtValue.Anonymous.doubleValue))
                    })
                    .collect())
            }
        }

        /// The clocks now. `gpu`: read the NVIDIA GPU too (it can wake it from sleep).
        pub fn read(&mut self, gpu: bool) -> Clocks {
            let mut out = Clocks::default();
            // The CPU clocks are averages since the last collect: give a read right after it a moment.
            let since = self.last.elapsed();
            if since < MIN_GAP {
                std::thread::sleep(MIN_GAP - since);
            }
            // SAFETY: the query from `new`.
            unsafe {
                PdhCollectQueryData(self.query);
            }
            self.last = std::time::Instant::now();
            match self.topology.clone() {
                Ok(t) => match (self.per_thread(self.perf), self.per_thread(self.freq)) {
                    (Ok(p), Ok(f)) => {
                        let rated = rated_from(&t, &f, &mut self.seen);
                        out.cpu = Some(cores_from(&t, &rated, &p));
                    }
                    (Err(e), _) | (_, Err(e)) => out.cpu_error = Some(e),
                },
                Err(e) => out.cpu_error = Some(e),
            }
            if gpu {
                let nv = self.nvml.get_or_insert_with(Nvml::open);
                match nv {
                    Ok(n) => out.gpu = Some(n.read()),
                    Err(e) => out.gpu_error = Some(e.clone()),
                }
            }
            // The energy meter reports mW per rail (rapl_package0_pkg, _pp0, _pp1, _dram).
            let rails: Vec<(String, f64)> = self.instances(self.energy).unwrap_or_default().into_iter().map(|(n, mw)| (n.to_ascii_lowercase(), mw / 1_000.0)).collect();
            let battery = self.battery_now();
            out.power = Some(power_from(&rails, out.gpu.as_ref(), battery));
            out
        }
    }

    impl Default for ClockReader {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for ClockReader {
        fn drop(&mut self) {
            // SAFETY: the query from `new`.
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's i7-12700H: 6 P-cores with 2 threads (0-11, class 1), 8 E-cores (12-19, class 0); P-cores
    /// rated 2300 MHz, E-cores 1700.
    fn i7_12700h() -> (Vec<(u8, Vec<u32>)>, Vec<(u32, u32)>) {
        let mut topo: Vec<(u8, Vec<u32>)> = (0..6).map(|c| (1u8, vec![c * 2, c * 2 + 1])).collect();
        topo.extend((12..20).map(|t| (0u8, vec![t])));
        let base = (0..20).map(|t| (t, if t < 12 { 2300 } else { 1700 })).collect();
        (topo, base)
    }

    #[test]
    fn cores_are_labelled_p_and_e_with_their_running_clocks() {
        let (topo, base) = i7_12700h();
        // P-core threads at 121.7 %, E-cores at 98.7 %: as measured on the owner's laptop, 2026-11-25.
        let perf: Vec<(u32, f64)> = (0..20).map(|t| (t, if t < 12 { 121.7 } else { 98.7 })).collect();
        let c = cores_from(&topo, &base, &perf);
        assert_eq!(c.cores.len(), 14);
        assert_eq!(c.cores.iter().map(|k| k.label.as_str()).collect::<Vec<_>>()[..8], ["P0", "P1", "P2", "P3", "P4", "P5", "E0", "E1"]);
        let p0 = &c.cores[0];
        assert_eq!((p0.threads.clone(), p0.base_mhz, p0.efficient), (vec![0, 1], 2300, false));
        assert!((p0.mhz - 2799.1).abs() < 0.5, "2300 x 121.7 %: {}", p0.mhz);
        let e0 = &c.cores[6];
        assert!(e0.efficient && (e0.mhz - 1677.9).abs() < 0.5, "1700 x 98.7 %: {}", e0.mhz);
        assert!((c.max_mhz - 2799.1).abs() < 0.5);
        assert!((c.avg_mhz - (6.0 * 2799.1 + 8.0 * 1677.9) / 14.0).abs() < 0.5);
    }

    #[test]
    fn a_core_averages_its_threads_and_idle_threads_count() {
        let (topo, base) = i7_12700h();
        let perf = vec![(0, 150.0), (1, 50.0)];
        let c = cores_from(&topo, &base, &perf);
        assert!((c.cores[0].mhz - 2300.0).abs() < 0.01, "(3450 + 1150) / 2");
        assert_eq!(c.cores[1].mhz, 0.0, "no reading: parked");
    }

    #[test]
    fn a_cpu_of_one_kind_is_c0_onward() {
        let topo = vec![(0u8, vec![0, 1]), (0, vec![2, 3])];
        let base = vec![(0, 3000), (1, 3000), (2, 3000), (3, 3000)];
        let c = cores_from(&topo, &base, &[(0, 100.0), (1, 100.0), (2, 100.0), (3, 100.0)]);
        assert_eq!(c.cores.iter().map(|k| (k.label.as_str(), k.efficient)).collect::<Vec<_>>(), [("C0", false), ("C1", false)]);
    }

    #[test]
    fn each_kind_of_core_is_rated_at_the_highest_frequency_its_threads_show() {
        let (topo, _) = i7_12700h();
        let mut seen = Vec::new();
        // As read on the owner's laptop: P-threads 2300 with an idle one at 1506, E-threads 1700 with dips to 1500.
        let mut freq: Vec<(u32, f64)> = (0..20).map(|t| (t, if t < 12 { 2300.0 } else { 1700.0 })).collect();
        freq[7].1 = 1506.0;
        freq[16].1 = 1500.0;
        let r = rated_from(&topo, &freq, &mut seen);
        assert_eq!((r[7], r[16]), ((7, 2300), (16, 1700)), "the dips don't count");
        // Later every E-thread dips: the class keeps what it has seen.
        let dipped: Vec<(u32, f64)> = (0..20).map(|t| (t, if t < 12 { 2300.0 } else { 1500.0 })).collect();
        let r = rated_from(&topo, &dipped, &mut seen);
        assert_eq!(r[19], (19, 1700));
    }

    fn rails(pkg: f64, pp0: f64) -> Vec<(String, f64)> {
        vec![("rapl_package0_dram".into(), 0.0), ("rapl_package0_pkg".into(), pkg), ("rapl_package0_pp0".into(), pp0), ("rapl_package0_pp1".into(), 0.0), ("_total".into(), 0.0)]
    }

    #[test]
    fn on_ac_the_parts_add_up_and_the_input_is_unknown() {
        // As read on the owner's laptop, 2026-11-25: package 41.85 W, cores 32.36 W, the rest 0; GPU 45 W; on AC,
        // full, not charging.
        let gpu = GpuClocks { power_w: Some(45.0), power_limit_w: Some(125.0), ..Default::default() };
        let p = power_from(&rails(41.85, 32.36), Some(&gpu), Some(BatteryNow { on_ac: true, charging_w: 0.0, discharging_w: 0.0 }));
        assert_eq!((p.cpu_package_w, p.cpu_cores_w, p.igpu_w, p.dram_w), (Some(41.85), Some(32.36), None, None), "0 W rails count as not measured");
        assert!((p.cpu_rest_w.unwrap() - 9.49).abs() < 1e-9);
        assert!((p.measured_w - 86.85).abs() < 1e-9);
        assert_eq!((p.input_w, p.output_w, p.rest_w), (None, None, None), "Windows doesn't report the adapter");
        assert_eq!(p.gpu_limit_w, Some(125.0));
    }

    #[test]
    fn on_battery_the_input_is_the_discharge_and_the_rest_is_what_nothing_measures() {
        let gpu = GpuClocks { power_w: Some(12.0), ..Default::default() };
        let p = power_from(&rails(20.0, 14.0), Some(&gpu), Some(BatteryNow { on_ac: false, charging_w: 0.0, discharging_w: 47.5 }));
        assert_eq!((p.input_w, p.output_w), (Some(47.5), Some(47.5)));
        assert!((p.rest_w.unwrap() - 15.5).abs() < 1e-9, "47.5 - (20 + 12)");
        // No battery reading: nothing known about the input.
        assert_eq!(power_from(&rails(20.0, 14.0), None, None).input_w, None);
    }

    #[test]
    fn instance_names_map_to_threads() {
        assert_eq!(thread_of("0,13"), Some(13));
        assert_eq!(thread_of("0,_Total"), None);
        assert_eq!(thread_of("_Total"), None);
        assert_eq!(thread_of("1,3"), None, "other processor groups aren't mapped");
    }
}
