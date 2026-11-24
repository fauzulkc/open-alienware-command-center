//! The boost key's listener (Fn+F1, PS/2 scancode 0x68 on the built-in keyboard, measured 2026-11-09). What a
//! press does (the target mode, who handles it, the debounce) is cockpit-core's `boostkey`, shared with the
//! command line's `cockpit boost-key target`.
//!
//! The same read-only Raw Input listener (`cockpit_core::keyinput`) also hands every key-down to the reactive
//! glow (`glow.rs`), which turns it into an LED id and heat in memory while the glow runs, and drops it
//! otherwise. Nothing logs or stores which keys were pressed. Keys are never blocked, injected or remapped.

pub use cockpit_core::boostkey::*;

#[cfg(windows)]
pub mod win {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    /// Whether the window in front belongs to an elevated process (unknown counts as elevated: then AWCC
    /// can't have seen the key either, because it can't even open that process).
    pub fn foreground_elevated() -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
            let mut tok = HANDLE::default();
            let elevated = if OpenProcessToken(h, TOKEN_QUERY, &mut tok).is_ok() {
                let mut e = TOKEN_ELEVATION::default();
                let mut n = 0u32;
                let ok = GetTokenInformation(tok, TokenElevation, Some((&mut e as *mut TOKEN_ELEVATION).cast()), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut n).is_ok();
                let _ = CloseHandle(tok);
                !ok || e.TokenIsElevated != 0
            } else {
                true
            };
            let _ = CloseHandle(h);
            elevated
        }
    }

    /// Listens on this thread forever (Raw Input, read-only): calls `on_press` for each key-down of the boost
    /// key, and hands every key-down (make code, flags) to `on_key` (the reactive glow).
    pub fn listen(mut on_press: impl FnMut() + 'static, mut on_key: impl FnMut(u16, u16) + 'static) -> anyhow::Result<()> {
        cockpit_core::keyinput::win::listen("AlienCockpitBoostKey", move |make, flags| {
            if make == super::SCANCODE {
                on_press();
            }
            on_key(make, flags);
        })
    }
}
