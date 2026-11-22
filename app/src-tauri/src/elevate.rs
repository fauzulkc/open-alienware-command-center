//! Live firmware readings need admin rights (AWCC's WMI class answers administrators only) or the hardware
//! service. Without the service, Alien Cockpit restarts itself as administrator: one UAC prompt, the same
//! one AWCC's own service avoids by running as SYSTEM.

/// The argument a restarted copy gets: `--after <pid>` waits for the old copy to exit first, so the
/// single-instance check doesn't hand the new copy back to the old one; it also means "don't ask again".
pub const AFTER: &str = "--after";

/// The pid to wait for, when this copy was started by a restart.
pub fn after_pid(args: &[String]) -> Option<u32> {
    let i = args.iter().position(|a| a == AFTER)?;
    args.get(i + 1)?.parse().ok()
}

/// Whether to ask for admin at start: the person wants live readings, this copy isn't admin, there's no
/// service to read through, and this copy isn't already the result of a restart (no asking twice).
pub fn should_ask(want: bool, elevated: bool, service: bool, restarted: bool) -> bool {
    want && !elevated && !service && !restarted
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut n = 0u32;
        let ok = GetTokenInformation(tok, TokenElevation, Some((&mut e as *mut TOKEN_ELEVATION).cast()), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut n).is_ok();
        let _ = CloseHandle(tok);
        ok && e.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Starts this exe again as administrator (UAC). Err when the person says no or Windows refuses;
/// the caller keeps running then.
#[cfg(windows)]
pub fn relaunch() -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let file = wide(&exe.to_string_lossy());
    let args = wide(&format!("{AFTER} {}", std::process::id()));
    let r = unsafe { ShellExecuteW(None, w!("runas"), PCWSTR(file.as_ptr()), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute returns a value above 32 on success.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err("Windows didn't start Alien Cockpit as administrator (the prompt was declined or blocked).".into())
    }
}

#[cfg(not(windows))]
pub fn relaunch() -> Result<(), String> {
    Err("admin restart is Windows only".into())
}

/// Waits up to `secs` for the old copy to exit.
#[cfg(windows)]
pub fn wait_for(pid: u32, secs: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, secs * 1000);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
pub fn wait_for(_pid: u32, _secs: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_argument() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(after_pid(&a(&["x.exe", "--after", "4242"])), Some(4242));
        assert_eq!(after_pid(&a(&["x.exe", "--after"])), None);
        assert_eq!(after_pid(&a(&["x.exe", "--after", "abc"])), None);
        assert_eq!(after_pid(&a(&["x.exe"])), None);
    }

    #[test]
    fn asks_once_and_only_when_it_helps() {
        assert!(should_ask(true, false, false, false));
        assert!(!should_ask(false, false, false, false), "setting off");
        assert!(!should_ask(true, true, false, false), "already admin");
        assert!(!should_ask(true, false, true, false), "the service reads instead");
        assert!(!should_ask(true, false, false, true), "a restarted copy never asks again");
    }
}
