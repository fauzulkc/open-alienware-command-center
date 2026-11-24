//! The daily log: one JSON-lines file per local day (`logs\2026-11-09.jsonl`) with a reading and the top
//! processes every `every_s` seconds. Cleaned up automatically: files older than `keep_days` go, then the
//! oldest go until the folder fits `max_mb`. Nothing leaves this machine.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LogSettings {
    pub enabled: bool,
    /// Seconds between logged readings: 10, 30, 60 or 300.
    pub every_s: u32,
    /// Days of logs kept: 1–365.
    pub keep_days: u32,
    /// Size cap for the whole log folder, MB: 20–2000.
    pub max_mb: u32,
}

/// The standard amount: a reading every 30 s for two weeks (about 2–4 MB a day), capped at 200 MB.
impl Default for LogSettings {
    fn default() -> Self {
        LogSettings { enabled: true, every_s: 30, keep_days: 14, max_mb: 200 }
    }
}

impl LogSettings {
    pub fn validate(&self) -> Result<(), String> {
        if ![10, 30, 60, 300].contains(&self.every_s) {
            return Err("log every 10, 30, 60 or 300 seconds".into());
        }
        if !(1..=365).contains(&self.keep_days) {
            return Err("keep logs for 1–365 days".into());
        }
        if !(20..=2000).contains(&self.max_mb) {
            return Err("the log size cap must be 20–2000 MB".into());
        }
        Ok(())
    }
}

pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("Alien Cockpit OSS").join("logs")
}

/// Today's local date as YYYY-MM-DD.
pub fn today() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
    }
    #[cfg(not(windows))]
    {
        let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
        civil(days)
    }
}

/// Days since 1970-01-01 → YYYY-MM-DD (proleptic Gregorian; Howard Hinnant's algorithm).
#[cfg_attr(windows, allow(dead_code))]
pub fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// YYYY-MM-DD → days since 1970-01-01 (the inverse of [`civil`]).
pub fn day_number(day: &str) -> Option<i64> {
    let mut parts = day.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn day_of(name: &str) -> Option<String> {
    let d = name.strip_suffix(".jsonl")?;
    (d.len() == 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-' && d.chars().filter(|c| c.is_ascii_digit()).count() == 8).then(|| d.to_string())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Day {
    pub day: String,
    pub bytes: u64,
}

/// Logged days, newest first.
pub fn days(dir: &Path) -> Vec<Day> {
    let mut out: Vec<Day> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let day = day_of(&e.file_name().to_string_lossy())?;
            Some(Day { day, bytes: e.metadata().map(|m| m.len()).unwrap_or(0) })
        })
        .collect();
    out.sort_by(|a, b| b.day.cmp(&a.day));
    out
}

/// Appends one reading to today's file.
pub fn append(dir: &Path, day: &str, point: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{day}.jsonl")))?;
    let mut line = serde_json::to_vec(point)?;
    line.push(b'\n');
    f.write_all(&line)
}

/// Deletes days older than `keep_days` calendar days (counting today), then the oldest until the folder fits
/// `max_mb`. Returns the days removed. Today's file is never removed. Age is by date, not by how many files
/// there are: after a few weeks away, old files still go.
pub fn cleanup(dir: &Path, today: &str, s: &LogSettings) -> Vec<String> {
    let all = days(dir);
    let mut removed = Vec::new();
    let mut kept: Vec<&Day> = Vec::new();
    let today_n = day_number(today);
    for d in all.iter() {
        let age = today_n.zip(day_number(&d.day)).map(|(t, n)| t - n).unwrap_or(0);
        if d.day != today && age >= s.keep_days as i64 {
            if std::fs::remove_file(dir.join(format!("{}.jsonl", d.day))).is_ok() {
                removed.push(d.day.clone());
            }
        } else {
            kept.push(d);
        }
    }
    let cap = s.max_mb as u64 * 1_048_576;
    let mut total: u64 = kept.iter().map(|d| d.bytes).sum();
    for d in kept.iter().rev() {
        if total <= cap || d.day == today {
            break;
        }
        if std::fs::remove_file(dir.join(format!("{}.jsonl", d.day))).is_ok() {
            total -= d.bytes;
            removed.push(d.day.clone());
        }
    }
    removed
}

/// A day's readings, evenly thinned to at most `max_points` so a whole day draws quickly.
pub fn read(dir: &Path, day: &str, max_points: usize) -> Vec<Value> {
    if day_of(&format!("{day}.jsonl")).is_none() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(dir.join(format!("{day}.jsonl"))).unwrap_or_default();
    let all: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if all.len() <= max_points || max_points == 0 {
        return all;
    }
    let step = all.len() as f64 / max_points as f64;
    (0..max_points).map(|i| all[(i as f64 * step) as usize].clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_from_days_since_the_epoch() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(20_766), "2026-11-09");
        assert_eq!(civil(19_782), "2024-02-29");
    }

    #[test]
    fn settings_are_bounded() {
        assert!(LogSettings::default().validate().is_ok());
        for bad in [LogSettings { every_s: 5, ..Default::default() }, LogSettings { keep_days: 0, ..Default::default() }, LogSettings { max_mb: 5000, ..Default::default() }] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn append_list_read_and_thin() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..100 {
            append(d.path(), "2026-11-09", &json!({ "t": i })).unwrap();
        }
        append(d.path(), "2026-11-08", &json!({ "t": 0 })).unwrap();
        std::fs::write(d.path().join("notes.txt"), "x").unwrap();
        let ds = days(d.path());
        assert_eq!(ds.iter().map(|x| x.day.as_str()).collect::<Vec<_>>(), vec!["2026-11-09", "2026-11-08"], "newest first, other files ignored");
        let thin = read(d.path(), "2026-11-09", 10);
        assert_eq!(thin.len(), 10);
        assert_eq!(thin[1]["t"], 10);
        assert!(read(d.path(), "../secret", 10).is_empty(), "only day names are read");
    }

    #[test]
    fn cleanup_by_age_then_by_size_never_today() {
        let d = tempfile::tempdir().unwrap();
        let big = "x".repeat(600_000);
        for day in ["2026-11-01", "2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05"] {
            std::fs::write(d.path().join(format!("{day}.jsonl")), &big).unwrap();
        }
        let removed = cleanup(d.path(), "2026-11-05", &LogSettings { keep_days: 3, max_mb: 2000, ..Default::default() });
        assert_eq!(removed, vec!["2026-11-02", "2026-11-01"]);
        let removed = cleanup(d.path(), "2026-11-05", &LogSettings { keep_days: 30, max_mb: 1, ..Default::default() });
        assert_eq!(removed, vec!["2026-11-03", "2026-11-04"], "oldest first until it fits; today stays even over the cap");
        assert_eq!(days(d.path()).len(), 1);
    }

    /// Age counts calendar days: after three weeks away, a file from before is older than 14 days even though
    /// only two files exist. A file dated in the future (a clock that was wrong) isn't "old".
    #[test]
    fn cleanup_counts_calendar_days_not_files() {
        let d = tempfile::tempdir().unwrap();
        for day in ["2026-10-10", "2026-11-04", "2026-12-01"] {
            std::fs::write(d.path().join(format!("{day}.jsonl")), "{}\n").unwrap();
        }
        let removed = cleanup(d.path(), "2026-11-05", &LogSettings { keep_days: 14, ..Default::default() });
        assert_eq!(removed, vec!["2026-10-10"]);
        assert_eq!(days(d.path()).iter().map(|x| x.day.as_str()).collect::<Vec<_>>(), vec!["2026-12-01", "2026-11-04"]);
    }

    #[test]
    fn day_numbers_invert_civil_dates() {
        for n in [0i64, 59, 60, 365, 10_957, 20_407, 20_772] {
            assert_eq!(day_number(&civil(n)), Some(n), "{}", civil(n));
        }
        assert_eq!(day_number("2026-11-05").unwrap() - day_number("2026-10-22").unwrap(), 14);
        assert_eq!(day_number("2026-13-01"), None);
        assert_eq!(day_number("nonsense"), None);
    }
}
