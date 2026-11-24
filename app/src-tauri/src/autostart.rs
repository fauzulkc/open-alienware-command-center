//! Start at sign-in: the Task Scheduler task that starts the app elevated in the tray. Its arguments and the
//! sign-in check are cockpit-core's `autostart`; creating or deleting the task (it starts this exe) is here.

pub use cockpit_core::autostart::*;

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
