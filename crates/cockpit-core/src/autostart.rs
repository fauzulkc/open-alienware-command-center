//! Start at sign-in. Alien Cockpit needs administrator rights for live readings, and a Run-key entry would ask
//! for them (one UAC prompt) at every sign-in; a Task Scheduler task "at log on, with highest privileges"
//! starts it elevated without asking. Creating that task needs administrator rights once, which the app has
//! whenever it reads the firmware directly. The task starts the app in the tray (`--autostart`), no window.
//! Only the person turns this on, from Settings. Creating the task is the app's (`app/src-tauri/src/autostart.rs`):
//! it starts the app's own exe.

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
