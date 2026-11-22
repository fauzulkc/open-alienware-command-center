//! Windows performance counters for CPU power and speed, without any driver (case P-07).

use anyhow::{bail, Result};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::System::Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY};

pub const COUNTERS: &[&str] = &[
    "\\Energy Meter(*)\\Power",
    "\\Processor Information(_Total)\\% Processor Performance",
    "\\Processor Information(_Total)\\Processor Frequency",
    "\\Processor Information(_Total)\\% Processor Time",
];

const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// (counter path, instance, value or why there is none).
pub type Row = (String, String, Result<f64, String>);

/// Every counter in `COUNTERS` after two samples `gap` apart.
pub fn sample(gap: Duration) -> Result<Vec<Row>> {
    sample_paths(COUNTERS, gap)
}

/// Any counter paths (wildcards allowed) after two samples `gap` apart. A counter that doesn't exist is
/// listed with the PDH error.
pub fn sample_paths(paths: &[&str], gap: Duration) -> Result<Vec<Row>> {
    unsafe {
        let mut q = PDH_HQUERY::default();
        let st = PdhOpenQueryW(PCWSTR::null(), 0, &mut q);
        if st != 0 {
            bail!("PdhOpenQuery failed: 0x{st:08X}");
        }
        let mut handles: Vec<(&str, Result<PDH_HCOUNTER, u32>)> = Vec::new();
        for path in paths {
            let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let mut c = PDH_HCOUNTER::default();
            let st = PdhAddEnglishCounterW(q, PCWSTR(w.as_ptr()), 0, &mut c);
            handles.push((path, if st == 0 { Ok(c) } else { Err(st) }));
        }
        PdhCollectQueryData(q);
        std::thread::sleep(gap);
        PdhCollectQueryData(q);
        let mut out = Vec::new();
        for (path, h) in handles {
            let c = match h {
                Ok(c) => c,
                Err(st) => {
                    out.push((path.to_string(), String::new(), Err(format!("not available (PDH 0x{st:08X})"))));
                    continue;
                }
            };
            let (mut bytes, mut count) = (0u32, 0u32);
            let st = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut bytes, &mut count, None);
            if st != PDH_MORE_DATA {
                out.push((path.to_string(), String::new(), Err(format!("no data (PDH 0x{st:08X})"))));
                continue;
            }
            let n = (bytes as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) + 1;
            let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); n];
            let st = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut bytes, &mut count, Some(buf.as_mut_ptr()));
            if st != 0 {
                out.push((path.to_string(), String::new(), Err(format!("no data (PDH 0x{st:08X})"))));
                continue;
            }
            for item in &buf[..count as usize] {
                let name = if item.szName.is_null() { String::new() } else { item.szName.to_string().unwrap_or_default() };
                let v = item.FmtValue;
                let val = if v.CStatus == 0 { Ok(v.Anonymous.doubleValue) } else { Err(format!("status 0x{:08X}", v.CStatus)) };
                out.push((path.to_string(), name, val));
            }
        }
        PdhCloseQuery(q);
        Ok(out)
    }
}
