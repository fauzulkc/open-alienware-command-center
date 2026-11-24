//! The boost key's listener (Fn+F1, PS/2 scancode 0x68 on the built-in keyboard, measured 2026-11-09). What a
//! press does (the target mode, who handles it, the debounce) is cockpit-core's `boostkey`, shared with the
//! command line's `cockpit boost-key target`.
//!
//! Privacy: only the scancode is compared; every other key is dropped without being stored or looked at.

pub use cockpit_core::boostkey::*;

#[cfg(windows)]
pub mod win {
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::Input::{GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, RegisterClassW, HWND_MESSAGE, MSG, RI_KEY_BREAK, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WNDCLASSW};

    thread_local! {
        static ON_KEY: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    }

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

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_INPUT {
            let hdr = std::mem::size_of::<RAWINPUTHEADER>() as u32;
            let mut raw = std::mem::zeroed::<RAWINPUT>();
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            let got = GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some((&mut raw as *mut RAWINPUT).cast()), &mut size, hdr);
            if got != u32::MAX && raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                let k = raw.data.keyboard;
                if k.MakeCode == super::SCANCODE && (k.Flags as u32 & RI_KEY_BREAK) == 0 {
                    ON_KEY.with(|f| {
                        if let Some(f) = f.borrow_mut().as_mut() {
                            f();
                        }
                    });
                }
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// Listens on this thread forever and calls `on_press` for each key-down of the boost key.
    pub fn listen(on_press: impl FnMut() + 'static) -> anyhow::Result<()> {
        ON_KEY.with(|f| *f.borrow_mut() = Some(Box::new(on_press)));
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            let class = w!("AlienCockpitBoostKey");
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() });
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None)?;
            RegisterRawInputDevices(&[RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd }], std::mem::size_of::<RAWINPUTDEVICE>() as u32)?;
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}
