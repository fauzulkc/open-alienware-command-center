//! Start at sign-in. Alien Cockpit needs administrator rights for live readings, and a Run-key entry would ask
//! for them (one UAC prompt) at every sign-in; a Task Scheduler task "at log on, with highest privileges"
//! starts it elevated without asking. Creating that task needs administrator rights once, which the app has
//! whenever it reads the firmware directly. The task starts the app in the tray (`--autostart`), no window.
//! Only the person turns this on, from Settings.

pub const TASK_NAME: &str = "Alien Cockpit OSS";
pub const AUTOSTART_ARG: &str = "--autostart";

/// The `schtasks.exe` arguments that create (or replace) or delete the task for this user.
pub fn schtasks_args(enable: bool, exe: &str, user: &str) -> Vec<String> {
    if enable {
        vec![
            "/Create".into(),
            "/TN".into(),
            TASK_NAME.into(),
            "/TR".into(),
            format!("\"{exe}\" {AUTOSTART_ARG}"),
            "/SC".into(),
            "ONLOGON".into(),
            "/RU".into(),
            user.into(),
            "/RL".into(),
            "HIGHEST".into(),
            "/F".into(),
        ]
    } else {
        vec!["/Delete".into(), "/TN".into(), TASK_NAME.into(), "/F".into()]
    }
}

/// Whether this start came from the sign-in task (start in the tray, no window).
pub fn started_at_sign_in(args: &[String]) -> bool {
    args.iter().any(|a| a == AUTOSTART_ARG)
}

/// Creates or deletes the task. Needs administrator rights.
#[cfg(windows)]
pub fn set(enable: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if !crate::elevate::is_elevated() {
        return Err("Starting at sign-in needs Alien Cockpit running as administrator once to set it up. Use \"Get live readings\" first.".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = std::env::var("USERDOMAIN").map(|d| format!("{d}\\")).unwrap_or_default() + &std::env::var("USERNAME").map_err(|e| e.to_string())?;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("schtasks.exe").args(schtasks_args(enable, &exe.to_string_lossy(), &user)).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| e.to_string())?;
    if out.status.success() || (!enable && String::from_utf8_lossy(&out.stderr).contains("cannot find")) {
        Ok(())
    } else {
        Err(format!("Task Scheduler said: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(not(windows))]
pub fn set(_enable: bool) -> Result<(), String> {
    Err("Starting at sign-in is Windows only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_starts_the_app_elevated_in_the_tray_at_sign_in() {
        let a = schtasks_args(true, r"D:\apps\alien-cockpit.exe", r"X17\owner");
        let has = |pair: [&str; 2]| a.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1]);
        assert!(has(["/SC", "ONLOGON"]) && has(["/RL", "HIGHEST"]) && has(["/RU", r"X17\owner"]) && has(["/TN", TASK_NAME]));
        assert!(has(["/TR", r#""D:\apps\alien-cockpit.exe" --autostart"#]), "{a:?}");
        assert_eq!(a.last().map(String::as_str), Some("/F"), "replaces an older task, e.g. after the exe moved");
        assert_eq!(schtasks_args(false, "x", "y"), vec!["/Delete", "/TN", TASK_NAME, "/F"]);
    }

    #[test]
    fn a_sign_in_start_is_recognised() {
        assert!(started_at_sign_in(&["alien-cockpit.exe".into(), "--autostart".into()]));
        assert!(!started_at_sign_in(&["alien-cockpit.exe".into(), "--after".into(), "12".into()]));
    }
}
