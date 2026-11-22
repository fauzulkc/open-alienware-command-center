//! Boost-key capture (case P-06): listen to the Alienware HID devices with Raw Input and record what arrives,
//! next to the firmware's current profile once a second.
//!
//! Privacy: reports from the laptop keyboard's *typing* keys (letters, digits, punctuation, space, Enter,
//! Backspace, Tab, numpad digits) are counted, never recorded. Everything else (vendor and consumer-control
//! reports, function, media and unmapped keys) is recorded, because the boost key is one of those.

use serde::Serialize;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDI_DEVICEINFO, RIDI_DEVICENAME, RID_DEVICE_INFO, RID_INPUT, RIM_TYPEHID,
    RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, RegisterClassW, SetTimer, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_TIMER, WNDCLASSW};

/// Vendor ids of the devices AWCC drives: the Alienware lighting/macro controller and the Darfon keyboard.
pub const VENDORS: &[&str] = &["VID_187C", "VID_0D62"];

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub name: String,
    pub kind: &'static str,
    pub usage_page: u16,
    pub usage: u16,
    #[serde(skip)]
    handle: isize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Hid { ms: u64, device: String, usage_page: u16, usage: u16, report: String },
    Key { ms: u64, device: String, make: u16, flags: u16, vkey: u16, message: u32 },
    Profile { ms: u64, profile: Option<String> },
}

#[derive(Debug, Default, Serialize)]
pub struct Capture {
    pub devices: Vec<Device>,
    pub events: Vec<Event>,
    /// Typing keys seen and dropped (privacy), as a count only.
    pub typing_keys_dropped: u64,
}

/// Keys that carry text: never recorded.
pub fn is_typing_key(vkey: u16) -> bool {
    matches!(vkey, 0x08 | 0x09 | 0x0D | 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x69 | 0x6A..=0x6F | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2)
}

pub fn devices() -> Vec<Device> {
    let mut out = Vec::new();
    unsafe {
        let size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
        let mut n = 0u32;
        GetRawInputDeviceList(None, &mut n, size);
        let mut list = vec![RAWINPUTDEVICELIST::default(); n as usize];
        let got = GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut n, size);
        if got == u32::MAX {
            return out;
        }
        for d in &list[..got as usize] {
            let mut len = 0u32;
            GetRawInputDeviceInfoW(Some(d.hDevice), RIDI_DEVICENAME, None, &mut len);
            let mut name = vec![0u16; len as usize + 1];
            GetRawInputDeviceInfoW(Some(d.hDevice), RIDI_DEVICENAME, Some(name.as_mut_ptr().cast()), &mut len);
            let name = String::from_utf16_lossy(&name).trim_end_matches('\0').to_string();
            // The built-in keyboard's PS/2 path (ACPI\MSFT0001) carries scancodes like the performance key
            // (0x68 per the Linux hwdb), so it is listened to as well; typing keys are still dropped.
            // Every keyboard is listened to: hotkeys can arrive through the Intel HID Event Filter's converted
            // keyboard (HID\CONVERTEDDEVICE), the PS/2 path (ACPI\MSFT0001) or the Darfon keyboard. Typing keys
            // are dropped whichever keyboard sends them. Other HID collections only from the Alienware vendors.
            let upper = name.to_ascii_uppercase();
            if d.dwType != RIM_TYPEKEYBOARD && !VENDORS.iter().any(|v| upper.contains(v)) {
                continue;
            }
            let mut info = RID_DEVICE_INFO { cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
            let mut isz = info.cbSize;
            GetRawInputDeviceInfoW(Some(d.hDevice), RIDI_DEVICEINFO, Some((&mut info as *mut RID_DEVICE_INFO).cast()), &mut isz);
            let (kind, page, usage) = if d.dwType == RIM_TYPEHID {
                ("hid", info.Anonymous.hid.usUsagePage, info.Anonymous.hid.usUsage)
            } else if d.dwType == RIM_TYPEKEYBOARD {
                ("keyboard", 0x01, 0x06)
            } else {
                continue;
            };
            out.push(Device { name, kind, usage_page: page, usage, handle: d.hDevice.0 as isize });
        }
    }
    out
}

struct State {
    start: Instant,
    devices: Vec<Device>,
    cap: Capture,
    poll: Option<Box<dyn FnMut() -> Option<String>>>,
    deadline: Instant,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    let mut size = 0u32;
                    let hdr = std::mem::size_of::<RAWINPUTHEADER>() as u32;
                    GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, None, &mut size, hdr);
                    let mut buf = vec![0u8; size as usize];
                    if GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some(buf.as_mut_ptr().cast()), &mut size, hdr) == u32::MAX || (size as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
                        return;
                    }
                    // The byte buffer isn't aligned for RAWINPUT and a keyboard packet is shorter than the struct:
                    // copy what arrived into a zeroed, aligned RAWINPUT instead of casting the pointer.
                    let mut raw = std::mem::zeroed::<RAWINPUT>();
                    let n = buf.len().min(std::mem::size_of::<RAWINPUT>());
                    std::ptr::copy_nonoverlapping(buf.as_ptr(), (&mut raw as *mut RAWINPUT).cast::<u8>(), n);
                    let raw = &raw;
                    let h = raw.header.hDevice.0 as isize;
                    let Some(dev) = st.devices.iter().find(|d| d.handle == h) else { return };
                    let ms = st.start.elapsed().as_millis() as u64;
                    if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                        let k = raw.data.keyboard;
                        if is_typing_key(k.VKey) {
                            st.cap.typing_keys_dropped += 1;
                        } else {
                            st.cap.events.push(Event::Key { ms, device: dev.name.clone(), make: k.MakeCode, flags: k.Flags, vkey: k.VKey, message: k.Message });
                        }
                    } else if raw.header.dwType == RIM_TYPEHID.0 {
                        let hid = raw.data.hid;
                        let total = (hid.dwSizeHid as usize).saturating_mul(hid.dwCount as usize);
                        let off = std::mem::offset_of!(RAWINPUT, data) + std::mem::size_of::<u32>() * 2;
                        let bytes = &buf[off.min(buf.len())..(off + total).min(buf.len())];
                        let report = bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
                        st.cap.events.push(Event::Hid { ms, device: dev.name.clone(), usage_page: dev.usage_page, usage: dev.usage, report });
                    }
                }
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER => {
            let done = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let Some(st) = s.as_mut() else { return true };
                let ms = st.start.elapsed().as_millis() as u64;
                let profile = st.poll.as_mut().and_then(|p| p());
                st.cap.events.push(Event::Profile { ms, profile });
                Instant::now() >= st.deadline
            });
            if done {
                let _ = KillTimer(Some(hwnd), 1);
                let _ = DestroyWindow(hwnd);
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Listens for `secs` seconds. `poll` reads the current firmware profile (None when AWCC can't be read).
pub fn capture(secs: u64, poll: Option<Box<dyn FnMut() -> Option<String>>>) -> anyhow::Result<Capture> {
    let devs = devices();
    if devs.is_empty() {
        anyhow::bail!("no Alienware HID devices (VID 187C / 0D62) found");
    }
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("WiregentHwProbeKeys");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None)?;
        let mut usages: Vec<(u16, u16)> = devs.iter().map(|d| (d.usage_page, d.usage)).collect();
        usages.sort();
        usages.dedup();
        let regs: Vec<RAWINPUTDEVICE> = usages.iter().map(|(p, u)| RAWINPUTDEVICE { usUsagePage: *p, usUsage: *u, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd }).collect();
        RegisterRawInputDevices(&regs, std::mem::size_of::<RAWINPUTDEVICE>() as u32)?;
        STATE.with(|s| {
            *s.borrow_mut() = Some(State { start: Instant::now(), devices: devs, cap: Capture::default(), poll, deadline: Instant::now() + Duration::from_secs(secs) });
        });
        SetTimer(Some(hwnd), 1, 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
        let _ = HANDLE::default();
        let st = STATE.with(|s| s.borrow_mut().take()).expect("state");
        let mut cap = st.cap;
        cap.devices = st.devices;
        Ok(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_keys_are_never_recorded_but_special_keys_are() {
        for vk in [0x41u16, 0x5A, 0x30, 0x39, 0x20, 0x0D, 0x08, 0x09, 0x60, 0x69, 0xBA, 0xDE] {
            assert!(is_typing_key(vk), "0x{vk:02X}");
        }
        for vk in [0x70u16, 0x7B, 0x7C, 0x87, 0xAD, 0xB3, 0xFF, 0x10, 0x11, 0x1B] {
            assert!(!is_typing_key(vk), "0x{vk:02X} (F-keys, F13-F24, media, unmapped, modifiers, Esc)");
        }
    }
}
