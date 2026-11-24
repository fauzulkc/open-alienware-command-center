//! Key-downs, read-only, for the reactive glow and the boost key: which x17 R2 LED a Raw Input key event
//! belongs to ([`led_for`], pure), and on Windows the listener itself ([`win::listen`]).
//!
//! The listener registers for the keyboard collection (usage page 1, usage 6) with `RIDEV_INPUTSINK`, so it
//! hears keys while another window is in front. It never blocks, injects or remaps a key: Raw Input is a copy
//! of the input, and every key still goes to the window in front untouched (the owner's PowerToys remap of
//! Right Ctrl to Tab keeps working). Key-ups are dropped. Nothing here logs or stores which keys were pressed:
//! a key-down becomes an LED id handed to the caller, and the glow turns it into heat in memory.
//!
//! The table is US layout, PS/2 set 1 make codes as Raw Input reports them (`RAWKEYBOARD.MakeCode`, with
//! `RI_KEY_E0` for the extended keys). Keys whose Fn layer sends another code light the key the person
//! pressed: Fn+Home is Print Screen (E0 37), Fn+End is Pause (an E1 sequence), Fn+Delete is Insert (E0 52),
//! Fn+Up / Fn+Down are Page Up / Page Down (E0 49 / E0 51), and Fn+F1, the boost key, is 0x68.
//!
//! No scancode, so never lit by a press: Fn on its own (the keyboard handles it), the space bar (no LED), and
//! the media keys (speaker mute, volume up and down, mic mute): they reach Windows as consumer-control
//! usages, not keyboard scancodes, so the keyboard collection never sees them. The fake shifts Windows wraps
//! around some extended keys (E0 2A, E0 36) are not keys and map to nothing.

use alienfx::x17;

/// `RAWKEYBOARD.Flags` bits (winuser.h).
pub const RI_KEY_BREAK: u16 = 0x01;
pub const RI_KEY_E0: u16 = 0x02;
pub const RI_KEY_E1: u16 = 0x04;

/// (make code, E0 prefix, x17 LED id).
pub const SCANCODES: &[(u16, bool, u8)] = &[
    // F-row.
    (0x01, false, 0), // Esc
    (0x3B, false, 1),
    (0x3C, false, 2),
    (0x3D, false, 3),
    (0x3E, false, 4),
    (0x3F, false, 5),
    (0x40, false, 6),
    (0x41, false, 7),
    (0x42, false, 8),
    (0x43, false, 9),
    (0x44, false, 10),
    (0x57, false, 11),
    (0x58, false, 12),
    (0x68, false, 1), // Fn+F1, the boost key
    (0x47, true, 13), // Home
    (0x37, true, 13), // Fn+Home: Print Screen
    (0x4F, true, 14), // End
    (0x53, true, 15), // Delete
    (0x52, true, 15), // Fn+Delete: Insert
    // Numbers.
    (0x29, false, 20), // `
    (0x02, false, 21),
    (0x03, false, 22),
    (0x04, false, 23),
    (0x05, false, 24),
    (0x06, false, 25),
    (0x07, false, 26),
    (0x08, false, 27),
    (0x09, false, 28),
    (0x0A, false, 29),
    (0x0B, false, 30),
    (0x0C, false, 31), // -
    (0x0D, false, 32), // =
    (0x0E, false, 35), // Backspace
    // QWER row.
    (0x0F, false, 40), // Tab
    (0x10, false, 42), // Q
    (0x11, false, 43),
    (0x12, false, 44),
    (0x13, false, 45),
    (0x14, false, 46),
    (0x15, false, 47),
    (0x16, false, 48),
    (0x17, false, 49),
    (0x18, false, 50),
    (0x19, false, 51), // P
    (0x1A, false, 52), // [
    (0x1B, false, 53), // ]
    (0x2B, false, 55), // \ |
    // ASDF row.
    (0x3A, false, 61), // Caps Lock
    (0x1E, false, 62), // A
    (0x1F, false, 63),
    (0x20, false, 64),
    (0x21, false, 65),
    (0x22, false, 66),
    (0x23, false, 67),
    (0x24, false, 68),
    (0x25, false, 69),
    (0x26, false, 70), // L
    (0x27, false, 71), // ;
    (0x28, false, 72), // '
    (0x1C, false, 74), // Enter
    // ZXCV row.
    (0x2A, false, 81), // Left Shift
    (0x2C, false, 83), // Z
    (0x2D, false, 84),
    (0x2E, false, 85),
    (0x2F, false, 86),
    (0x30, false, 87),
    (0x31, false, 88),
    (0x32, false, 89), // M
    (0x33, false, 90), // ,
    (0x34, false, 91), // .
    (0x35, false, 92), // /
    (0x36, false, 94), // Right Shift
    (0x48, true, 114), // Up
    (0x49, true, 114), // Fn+Up: Page Up
    // Bottom row.
    (0x1D, false, 100), // Left Ctrl
    (0x5B, true, 103),  // Left Windows
    (0x38, false, 104), // Left Alt
    (0x38, true, 111),  // Right Alt
    (0x5C, true, 109),  // the Windows key on the right (Windows lock)
    (0x1D, true, 112),  // Right Ctrl (the owner's Tab, through PowerToys)
    (0x4B, true, 133),  // Left
    (0x50, true, 134),  // Down
    (0x51, true, 134),  // Fn+Down: Page Down
    (0x4D, true, 135),  // Right
];

/// Pause (Fn+End) comes as E1 1D 45; Raw Input reports it as make code 0x1D with `RI_KEY_E1`.
const PAUSE_E1: u16 = 0x1D;
const END: u8 = 14;

/// The LED of a key event (`MakeCode`, `Flags`), if the key has one.
pub fn led_for(make: u16, flags: u16) -> Option<u8> {
    if flags & RI_KEY_E1 != 0 {
        return (make == PAUSE_E1).then_some(END);
    }
    let e0 = flags & RI_KEY_E0 != 0;
    SCANCODES.iter().find(|(m, e, _)| *m == make && *e == e0).map(|x| x.2)
}

/// A key-down (not a key-up).
pub fn is_down(flags: u16) -> bool {
    flags & RI_KEY_BREAK == 0
}

/// Every LED a key-down can light (each present on this keyboard).
pub fn mapped_leds() -> impl Iterator<Item = u8> {
    SCANCODES.iter().map(|x| x.2).chain(Some(END)).filter(|id| x17::key(*id).is_some_and(|k| k.present))
}

#[cfg(windows)]
pub mod win {
    use std::cell::RefCell;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::{GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WNDCLASSW};

    type OnKey = Option<Box<dyn FnMut(u16, u16)>>;

    thread_local! {
        static ON_KEY: RefCell<OnKey> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_INPUT {
            let hdr = std::mem::size_of::<RAWINPUTHEADER>() as u32;
            let mut raw = std::mem::zeroed::<RAWINPUT>();
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            let got = GetRawInputData(HRAWINPUT(lp.0 as _), RID_INPUT, Some((&mut raw as *mut RAWINPUT).cast()), &mut size, hdr);
            if got != u32::MAX && raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                let k = raw.data.keyboard;
                if super::is_down(k.Flags) {
                    ON_KEY.with(|f| {
                        if let Some(f) = f.borrow_mut().as_mut() {
                            f(k.MakeCode, k.Flags);
                        }
                    });
                }
            }
        }
        // Raw Input is a copy: the key itself goes on to the window in front whatever happens here.
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// Listens on this thread forever (a message-only window named `class`) and calls `on_down(make, flags)`
    /// for each key-down on any keyboard, also while another window is in front. Read-only.
    pub fn listen(class: &str, on_down: impl FnMut(u16, u16) + 'static) -> anyhow::Result<()> {
        ON_KEY.with(|f| *f.borrow_mut() = Some(Box::new(on_down)));
        let name: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            let class = PCWSTR(name.as_ptr());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn name(make: u16, flags: u16) -> Option<&'static str> {
        led_for(make, flags).map(|id| x17::key(id).unwrap().name)
    }

    #[test]
    fn a_representative_set_maps_to_the_right_keys() {
        assert_eq!(name(0x01, 0), Some("ESC"));
        assert_eq!(name(0x3B, 0), Some("F1"));
        assert_eq!(name(0x44, 0), Some("F10"));
        assert_eq!(name(0x57, 0), Some("F11"));
        assert_eq!(name(0x58, 0), Some("F12"));
        assert_eq!(name(0x68, 0), Some("F1"), "Fn+F1, the boost key");
        assert_eq!(name(0x29, 0), Some("`"));
        assert_eq!(name(0x02, 0), Some("1"));
        assert_eq!(name(0x0B, 0), Some("0"));
        assert_eq!(name(0x0E, 0), Some("Backspace"));
        assert_eq!(name(0x0F, 0), Some("Tab"));
        assert_eq!(name(0x10, 0), Some("Q"));
        assert_eq!(name(0x2B, 0), Some("|"));
        assert_eq!(name(0x3A, 0), Some("Caps Lock"));
        assert_eq!(name(0x1E, 0), Some("A"));
        assert_eq!(name(0x28, 0), Some("'"));
        assert_eq!(name(0x1C, 0), Some("Enter"));
        assert_eq!(name(0x2A, 0), Some("Left Shift"));
        assert_eq!(name(0x2C, 0), Some("Z"));
        assert_eq!(name(0x35, 0), Some("/"));
        assert_eq!(name(0x36, 0), Some("Right Shift"));
        assert_eq!(name(0x1D, 0), Some("Left Ctrl"));
        assert_eq!(name(0x1D, RI_KEY_E0), Some("Right Ctrl"), "E0 1D: the owner's Tab through PowerToys");
        assert_eq!(name(0x38, 0), Some("Left Alt"));
        assert_eq!(name(0x38, RI_KEY_E0), Some("Right Alt"));
        assert_eq!(name(0x5B, RI_KEY_E0), Some("Left Windows"));
        assert_eq!(name(0x48, RI_KEY_E0), Some("Up / Page Up"));
        assert_eq!(name(0x49, RI_KEY_E0), Some("Up / Page Up"), "Fn+Up");
        assert_eq!(name(0x4B, RI_KEY_E0), Some("Left Arrow"));
        assert_eq!(name(0x50, RI_KEY_E0), Some("Down / Page Down"));
        assert_eq!(name(0x4D, RI_KEY_E0), Some("Right Arrow"));
        assert_eq!(name(0x47, RI_KEY_E0), Some("Home"));
        assert_eq!(name(0x4F, RI_KEY_E0), Some("End"));
        assert_eq!(name(0x53, RI_KEY_E0), Some("Delete"));
        assert_eq!(name(0x1D, RI_KEY_E1), Some("End"), "Fn+End: Pause");
        // The E0 prefix matters: E0 48 is Up, 48 alone is keypad 8 (no keypad here).
        assert_eq!(led_for(0x48, 0), None);
        assert_eq!(led_for(0x47, 0), None);
        assert_eq!(led_for(0x1C, RI_KEY_E0), None, "keypad Enter");
        // Key-ups carry the break flag; the mapping ignores it, the listener drops them.
        assert_eq!(name(0x10, RI_KEY_BREAK), Some("Q"));
        assert!(is_down(0) && is_down(RI_KEY_E0) && !is_down(RI_KEY_BREAK) && !is_down(RI_KEY_BREAK | RI_KEY_E0));
    }

    #[test]
    fn no_scancode_no_led_and_nothing_that_isnt_there() {
        assert_eq!(led_for(0x39, 0), None, "the space bar has no LED");
        assert_eq!(led_for(0x2A, RI_KEY_E0), None, "a fake shift");
        assert_eq!(led_for(0x36, RI_KEY_E0), None, "a fake shift");
        assert_eq!(led_for(0x56, 0), None, "the ISO key isn't on this keyboard");
        assert_eq!(led_for(0x45, RI_KEY_E1), None);
        for (m, e, id) in SCANCODES {
            assert!(x17::key(*id).is_some_and(|k| k.present), "{m:#04x} (E0 {e}) -> {id}: not a key here");
        }
        assert!(mapped_leds().all(|id| x17::key(id).unwrap().present));
        let pairs: BTreeSet<(u16, bool)> = SCANCODES.iter().map(|(m, e, _)| (*m, *e)).collect();
        assert_eq!(pairs.len(), SCANCODES.len(), "each scancode once");
        // Every lit key but Fn and the four media keys can be reached.
        let reached: BTreeSet<u8> = mapped_leds().collect();
        let unreached: Vec<&str> = x17::KEYS.iter().filter(|k| k.present && !reached.contains(&k.id)).map(|k| k.name).collect();
        assert_eq!(unreached, vec!["Microphone Mute", "Speaker Mute", "Volume Up", "Volume Down", "Fn"]);
    }
}
