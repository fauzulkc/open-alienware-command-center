//! The Dell drift check (plan D1): Dell's services against the owner's rules, after every boot. It reports;
//! it never changes a service. Re-applying goes through the System hub's apply-with-undo.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Rule {
    pub service: String,
    /// Start types that are fine: "Delayed", "Auto", "Manual", "Trigger", "Disabled".
    pub expect: Vec<String>,
    pub severity: String,
    pub why: String,
    #[serde(default)]
    pub critical_if: Vec<String>,
    #[serde(default)]
    pub critical_why: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Rules {
    pub rules: Vec<Rule>,
}

impl Rules {
    pub fn parse(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("parsing the drift rules")
    }

    /// The rules shipped with the plugin.
    pub fn builtin() -> Self {
        Self::parse(include_str!("../rules/drift.json")).expect("shipped drift rules parse")
    }
}

/// A service as the system reports it now.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub name: String,
    pub start: String,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Drift {
    pub service: String,
    pub expected: Vec<String>,
    /// The start type now, or "not installed".
    pub actual: String,
    pub running: bool,
    /// "critical", "high", "medium", "low", or "info" (not installed).
    pub severity: String,
    pub why: String,
}

pub fn check(rules: &Rules, live: &[Live]) -> Vec<Drift> {
    let mut out = Vec::new();
    for r in &rules.rules {
        let Some(l) = live.iter().find(|l| l.name.eq_ignore_ascii_case(&r.service)) else {
            out.push(Drift { service: r.service.clone(), expected: r.expect.clone(), actual: "not installed".into(), running: false, severity: "info".into(), why: format!("{} isn't installed; the rule has nothing to check.", r.service) });
            continue;
        };
        if r.expect.iter().any(|e| e.eq_ignore_ascii_case(&l.start)) {
            continue;
        }
        let critical = r.critical_if.iter().any(|c| c.eq_ignore_ascii_case(&l.start));
        out.push(Drift {
            service: r.service.clone(),
            expected: r.expect.clone(),
            actual: l.start.clone(),
            running: l.running,
            severity: if critical { "critical".into() } else { r.severity.clone() },
            why: if critical { r.critical_why.clone().unwrap_or_else(|| r.why.clone()) } else { r.why.clone() },
        });
    }
    let rank = |s: &str| ["critical", "high", "medium", "low", "info"].iter().position(|x| *x == s).unwrap_or(5);
    out.sort_by_key(|d| rank(&d.severity));
    out
}

/// The live state of the services the built-in rules name. A service that isn't installed is left out
/// (`check` reports it as "not installed"). Read-only: the service manager is opened to connect and query.
#[cfg(windows)]
pub fn live() -> Vec<Live> {
    Rules::builtin().rules.iter().filter_map(|r| service_state(&r.service)).collect()
}

/// One service's start type as Wiregent names them (Delayed, Auto, Manual, Trigger, Disabled) and whether it runs.
#[cfg(windows)]
fn service_state(name: &str) -> Option<Live> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::*;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).ok()?;
        let h = OpenServiceW(scm, PCWSTR(wide.as_ptr()), SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS);
        let Ok(h) = h else {
            let _ = CloseServiceHandle(scm);
            return None;
        };
        let mut status = SERVICE_STATUS::default();
        let running = QueryServiceStatus(h, &mut status).is_ok() && status.dwCurrentState == SERVICE_RUNNING;
        // QUERY_SERVICE_CONFIGW is followed by its strings; 8 KB is far more than any service needs.
        let mut cfg = vec![0u64; 1024];
        let mut need = 0u32;
        let start_type = if QueryServiceConfigW(h, Some(cfg.as_mut_ptr().cast()), (cfg.len() * 8) as u32, &mut need).is_ok() {
            (*(cfg.as_ptr() as *const QUERY_SERVICE_CONFIGW)).dwStartType
        } else {
            SERVICE_DEMAND_START
        };
        let mut b2 = vec![0u8; 8192];
        let delayed = QueryServiceConfig2W(h, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, Some(&mut b2), &mut need).is_ok() && (*(b2.as_ptr() as *const SERVICE_DELAYED_AUTO_START_INFO)).fDelayedAutostart.as_bool();
        let triggers = if QueryServiceConfig2W(h, SERVICE_CONFIG_TRIGGER_INFO, Some(&mut b2), &mut need).is_ok() { (*(b2.as_ptr() as *const SERVICE_TRIGGER_INFO)).cTriggers } else { 0 };
        let _ = CloseServiceHandle(h);
        let _ = CloseServiceHandle(scm);
        let start = match start_type {
            SERVICE_AUTO_START if delayed => "Delayed",
            SERVICE_AUTO_START | SERVICE_BOOT_START | SERVICE_SYSTEM_START => "Auto",
            SERVICE_DISABLED => "Disabled",
            _ if triggers > 0 => "Trigger",
            _ => "Manual",
        };
        Some(Live { name: name.to_string(), start: start.into(), running })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, start: &str) -> Live {
        Live { name: name.into(), start: start.into(), running: true }
    }

    /// R-11: the state measured on 2026-11-09 shows the one drift that was real that day.
    #[test]
    fn the_drift_measured_on_2026_11_09() {
        let live = vec![svc("DellTechHub", "Auto"), svc("SupportAssistAgent", "Delayed"), svc("DellClientManagementService", "Delayed"), Live { name: "Alienware SupportAssist Remediation".into(), start: "Manual".into(), running: false }];
        let d = check(&Rules::builtin(), &live);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].service.as_str(), d[0].actual.as_str(), d[0].severity.as_str()), ("DellTechHub", "Auto", "high"));
    }

    /// The service reader (own code since the move out of Wiregent): read-only, only the rules' services,
    /// and every start type is one of the five names the rules use.
    #[cfg(windows)]
    #[test]
    fn live_reads_only_the_rules_services_with_known_start_types() {
        let names: Vec<String> = Rules::builtin().rules.iter().map(|r| r.service.to_lowercase()).collect();
        for l in live() {
            assert!(names.contains(&l.name.to_lowercase()), "{l:?}");
            assert!(["Delayed", "Auto", "Manual", "Trigger", "Disabled"].contains(&l.start.as_str()), "{l:?}");
        }
    }

    /// R-12: TechHub on Manual is critical, with the reason; rules are sorted worst first.
    #[test]
    fn techhub_on_manual_is_critical() {
        let live = vec![svc("DellTechHub", "Manual"), svc("SupportAssistAgent", "Auto"), svc("DellClientManagementService", "Delayed")];
        let d = check(&Rules::builtin(), &live);
        assert_eq!(d[0].service, "DellTechHub");
        assert_eq!(d[0].severity, "critical");
        assert!(d[0].why.contains("boost key"));
        assert_eq!(d[1].service, "SupportAssistAgent");
        assert_eq!(d.last().unwrap().actual, "not installed", "Remediation missing is info, last");
    }

    #[test]
    fn matching_is_case_insensitive_and_clean_state_is_silent() {
        let live = vec![svc("delltechhub", "delayed"), svc("SupportAssistAgent", "Delayed"), svc("DellClientManagementService", "Delayed"), svc("Alienware SupportAssist Remediation", "Manual")];
        assert!(check(&Rules::builtin(), &live).is_empty());
    }
}
