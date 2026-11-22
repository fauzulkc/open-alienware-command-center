//! The write log (C-03/C-04): every write a client asks for and every write the service makes on its own
//! (the emergency rule, the watchdog's restore, the fan reset at start and stop) is one line with the time,
//! who (process id and program name, read from the process, never from the message), what, the value and
//! how it went. Lines go to the service's log callback; tests pass a sink that keeps them.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use wiregent_hw::client_identity::Client;

/// Where write lines go: the service's log in production, a Vec in tests.
pub type Sink = Arc<dyn Fn(String) + Send + Sync>;

/// The service's program name (`wiregent_hw::client::SERVICE_IMAGE`, which only builds on Windows).
const SERVICE_IMAGE: &str = "wiregent-hw.exe";

/// Who asked for a write.
pub enum By<'a> {
    /// A pipe client, as the service identified it.
    Client(&'a Client),
    /// The service's own safety rules: no client asked.
    Service,
}

/// Client-supplied text in a log line: no control characters (one line stays one line), at most 64 chars.
fn clean(s: &str) -> String {
    let mut out: String = s.chars().take(64).map(|c| if c.is_control() { '?' } else { c }).collect();
    if s.chars().count() > 64 {
        out.push('…');
    }
    out
}

/// UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn utc(t: SystemTime) -> String {
    let ms = t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rest / 3_600, rest % 3_600 / 60, rest % 60)
}

/// One write line. `op` and `value` say what was asked; `outcome` how it went.
pub fn line(at: SystemTime, by: By, op: &str, value: &str, outcome: &str) -> String {
    let who = match by {
        By::Client(c) => format!("by=client pid={} image={}", c.pid, c.image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| c.image.display().to_string())),
        By::Service => format!("by=service pid={} image={} (the service's own safety write)", std::process::id(), SERVICE_IMAGE),
    };
    format!("write {} {who} op={op} value={} outcome={}", utc(at), clean(value), clean_outcome(outcome))
}

/// Outcomes may be longer than values (an error's cause chain), but still one line.
fn clean_outcome(s: &str) -> String {
    s.chars().take(300).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn utc_formats_known_times() {
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc(UNIX_EPOCH + Duration::from_millis(1_795_352_645_123)), "2026-11-22T13:04:05.123Z");
        assert_eq!(utc(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn a_line_names_the_caller_and_stays_one_line() {
        let c = Client { pid: 77, image: PathBuf::from(r"C:\Program Files\Wiregent\wiregent.exe"), sid: "S-1-5-21-1".into() };
        let l = line(UNIX_EPOCH, By::Client(&c), "set-profile", "0xA1\nwrite forged by=service", "ok");
        assert_eq!(l, "write 1970-01-01T00:00:00.000Z by=client pid=77 image=wiregent.exe op=set-profile value=0xA1?write forged by=service outcome=ok");
        let s = line(UNIX_EPOCH, By::Service, "emergency-engage", "every fan 100", "ok");
        assert!(s.contains(&format!("by=service pid={} image=wiregent-hw.exe (the service's own safety write)", std::process::id())), "{s}");
    }
}
