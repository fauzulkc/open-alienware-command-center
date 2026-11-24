//! The boost key (Fn+F1, PS/2 scancode 0x68 on the built-in keyboard, measured 2026-11-09): what a press
//! does. The key listener itself is the app's (`app/src-tauri/src/boostkey.rs`).
//!
//! AWCC's listener is a normal-level program, so Windows keeps the key from it while an admin window is in
//! front (that's why the key "didn't work" with an elevated terminal or this app in front). Alien Cockpit
//! catches the key itself exactly then, when the window in front is elevated, or whenever AWCC isn't
//! running. Otherwise it leaves the key to AWCC, so the two never both toggle.
//!
//! Privacy: only the scancode is compared; every other key is dropped without being stored or looked at.

use std::time::{Duration, Instant};

pub const SCANCODE: u16 = 0x68;
pub const DEBOUNCE: Duration = Duration::from_millis(2_000);

/// What to do with a key-down of 0x68, given who would otherwise handle it.
pub fn should_handle(foreground_elevated: bool, awcc_running: bool) -> bool {
    foreground_elevated || !awcc_running
}

/// AWCC-style debounce: one toggle per press, presses within 2 s of the last toggle ignored.
#[derive(Default)]
pub struct Debounce {
    last: Option<Instant>,
}

impl Debounce {
    pub fn accept(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|l| now.duration_since(l) < DEBOUNCE) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The mode the key switches to, AWCC's way: into G-Mode, or back to the mode it left (or the default).
pub fn toggle_target(current: Option<u8>, previous: Option<u8>, default_profile: u8) -> u8 {
    if current == Some(wiregent_oem_alienware::GMODE) {
        previous.filter(|p| *p != wiregent_oem_alienware::GMODE).unwrap_or(default_profile)
    } else {
        wiregent_oem_alienware::GMODE
    }
}

/// The "step through modes" order: the everyday modes, quietest to loudest, then round again. Full Speed and
/// Battery Saver are left out (one is loud, the other slow) and stay a click away in the app.
pub const CYCLE: [u8; 4] = [0xA3, 0xA0, 0xA1, wiregent_oem_alienware::GMODE];

/// The next mode in [`CYCLE`]; from a mode outside it (or none read), Balanced.
pub fn cycle_target(current: Option<u8>) -> u8 {
    match current.and_then(|c| CYCLE.iter().position(|m| *m == c)) {
        Some(i) => CYCLE[(i + 1) % CYCLE.len()],
        None => wiregent_oem_alienware::BALANCED,
    }
}

/// What a press does, by the person's "boost key" setting: "cycle" steps through modes, anything else toggles
/// G-Mode like AWCC.
pub fn key_target(behaviour: &str, current: Option<u8>, previous: Option<u8>, default_profile: u8) -> u8 {
    if behaviour == "cycle" {
        cycle_target(current)
    } else {
        toggle_target(current, previous, default_profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alien_cockpit_only_takes_the_key_where_awcc_cant() {
        assert!(should_handle(true, true), "admin window in front: AWCC is blind, we take it");
        assert!(!should_handle(false, true), "normal window and AWCC running: AWCC takes it");
        assert!(should_handle(false, false), "no AWCC: always ours");
    }

    #[test]
    fn the_key_toggles_g_mode_and_back_like_awcc() {
        assert_eq!(toggle_target(Some(0xA0), None, 0xA0), 0xAB);
        assert_eq!(toggle_target(Some(0xAB), Some(0xA1), 0xA0), 0xA1, "back to the mode it left");
        assert_eq!(toggle_target(Some(0xAB), None, 0xA3), 0xA3, "or the default");
        assert_eq!(toggle_target(Some(0xAB), Some(0xAB), 0xA0), 0xA0);
    }

    /// The "step through modes" setting: Quiet → Balanced → Performance → G-Mode → Quiet.
    #[test]
    fn the_cycle_setting_steps_through_modes() {
        let mut m = Some(0xA3);
        let mut seen = Vec::new();
        for _ in 0..5 {
            let next = key_target("cycle", m, None, 0xA0);
            seen.push(next);
            m = Some(next);
        }
        assert_eq!(seen, vec![0xA0, 0xA1, 0xAB, 0xA3, 0xA0]);
        assert_eq!(cycle_target(Some(0xA4)), 0xA0, "from Full Speed: Balanced");
        assert_eq!(cycle_target(None), 0xA0, "no reading: Balanced");
        assert_eq!(key_target("awcc", Some(0xA0), None, 0xA0), 0xAB, "the AWCC setting still toggles G-Mode");
    }

    #[test]
    fn presses_within_two_seconds_are_ignored() {
        let mut d = Debounce::default();
        let t0 = Instant::now();
        assert!(d.accept(t0));
        assert!(!d.accept(t0 + Duration::from_millis(1_800)));
        assert!(d.accept(t0 + Duration::from_millis(2_100)));
    }
}
