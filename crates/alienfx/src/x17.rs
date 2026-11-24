//! The Alienware x17 R2 Cherry mechanical keyboard (USB 0D62:CABC): its LEDs, their names, AWCC's key
//! groups and a laptop layout map for drawing them.
//!
//! Ids, names and groups are AWCC's own description of this keyboard (FXMetadata
//! `0x0406_AdvCherryMechanicalKB_1.0.0.0.json`, `LigthingInfo.Leds` and `.Groups`), parsed once and committed
//! here; nothing reads that file at build or test time. AWCC's LED ids are the keyboard's v5 key indices
//! (L0 test, 2026-11-24).
//!
//! AWCC lists 91 ids; the owner's keyboard (photo, 2026-11-24) has 84 lit keys. The other 7 (JP1, JP3, UK1,
//! JP6, JP4 and a second Page Up / Page Down) have no key here: they stay in the table with
//! `present: false`, because writes cover AWCC's full 91 as AWCC's do. Names follow the keycaps where they
//! differ from AWCC's: 114 "Up / Page Up", 134 "Down / Page Down" (AWCC calls it "Page Down"), 109
//! "Windows lock". That the arrows are 114 and 134 is read from the photo's colours (AWCC's preset shows
//! them in the #FFCDE9 group, not 113/115's magenta); still to be confirmed on the device.
//!
//! The layout map: six rows (F-row, numbers, QWER, ASDF, ZXCV, bottom with the arrows) as on the photo,
//! columns and widths in quarter-key units (4 = one key), every row 64 quarters (16 keys) wide. The space
//! bar (quarters 17..37 of the bottom row) has no LED and no id: it is a gap here.
#![forbid(unsafe_code)]

/// One key: its LED id (the v5 index), its name, and where it sits on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyDef {
    pub id: u8,
    pub name: &'static str,
    /// 0 = F-row … 5 = bottom row; [`ABSENT_ROW`] for an id with no key on this keyboard.
    pub row: u8,
    /// Left edge, quarter-key units from the left of the row.
    pub col: u8,
    /// Width, quarter-key units (4 = one key); 0 when not present.
    pub width: u8,
    /// The key exists on this keyboard (84 of AWCC's 91 ids).
    pub present: bool,
}

const fn k(id: u8, name: &'static str, row: u8, col: u8, width: u8) -> KeyDef {
    KeyDef { id, name, row, col, width, present: true }
}

const fn absent(id: u8, name: &'static str) -> KeyDef {
    KeyDef { id, name, row: ABSENT_ROW, col: 0, width: 0, present: false }
}

/// Rows on the map.
pub const ROWS: u8 = 6;
/// The row of an id with no key on this keyboard.
pub const ABSENT_ROW: u8 = 255;
/// Width of every row, quarter-key units.
pub const ROW_WIDTH: u8 = 64;
/// The space bar on the bottom row (col, width), quarter units: no LED, drawn unlit.
pub const SPACE_BAR: (u8, u8) = (17, 20);

/// AWCC's 91 ids: the 84 keys row by row, left to right, then the 7 with no key here.
pub const KEYS: [KeyDef; 91] = [
    k(0, "ESC", 0, 0, 4),
    k(1, "F1", 0, 4, 4),
    k(2, "F2", 0, 8, 4),
    k(3, "F3", 0, 12, 4),
    k(4, "F4", 0, 16, 4),
    k(5, "F5", 0, 20, 4),
    k(6, "F6", 0, 24, 4),
    k(7, "F7", 0, 28, 4),
    k(8, "F8", 0, 32, 4),
    k(9, "F9", 0, 36, 4),
    k(10, "F10", 0, 40, 4),
    k(11, "F11", 0, 44, 4),
    k(12, "F12", 0, 48, 4),
    k(13, "Home", 0, 52, 4),
    k(14, "End", 0, 56, 4),
    k(15, "Delete", 0, 60, 4),
    k(20, "`", 1, 0, 4),
    k(21, "1", 1, 4, 4),
    k(22, "2", 1, 8, 4),
    k(23, "3", 1, 12, 4),
    k(24, "4", 1, 16, 4),
    k(25, "5", 1, 20, 4),
    k(26, "6", 1, 24, 4),
    k(27, "7", 1, 28, 4),
    k(28, "8", 1, 32, 4),
    k(29, "9", 1, 36, 4),
    k(30, "0", 1, 40, 4),
    k(31, "-", 1, 44, 4),
    k(32, "=", 1, 48, 4),
    k(35, "Backspace", 1, 52, 8),
    k(19, "Microphone Mute", 1, 60, 4),
    k(40, "Tab", 2, 0, 6),
    k(42, "Q", 2, 6, 4),
    k(43, "W", 2, 10, 4),
    k(44, "E", 2, 14, 4),
    k(45, "R", 2, 18, 4),
    k(46, "T", 2, 22, 4),
    k(47, "Y", 2, 26, 4),
    k(48, "U", 2, 30, 4),
    k(49, "I", 2, 34, 4),
    k(50, "O", 2, 38, 4),
    k(51, "P", 2, 42, 4),
    k(52, "[", 2, 46, 4),
    k(53, "]", 2, 50, 4),
    k(55, "|", 2, 54, 6),
    k(16, "Speaker Mute", 2, 60, 4),
    k(61, "Caps Lock", 3, 0, 7),
    k(62, "A", 3, 7, 4),
    k(63, "S", 3, 11, 4),
    k(64, "D", 3, 15, 4),
    k(65, "F", 3, 19, 4),
    k(66, "G", 3, 23, 4),
    k(67, "H", 3, 27, 4),
    k(68, "J", 3, 31, 4),
    k(69, "K", 3, 35, 4),
    k(70, "L", 3, 39, 4),
    k(71, ";", 3, 43, 4),
    k(72, "'", 3, 47, 4),
    k(74, "Enter", 3, 51, 9),
    k(18, "Volume Up", 3, 60, 4),
    k(81, "Left Shift", 4, 0, 9),
    k(83, "Z", 4, 9, 4),
    k(84, "X", 4, 13, 4),
    k(85, "C", 4, 17, 4),
    k(86, "V", 4, 21, 4),
    k(87, "B", 4, 25, 4),
    k(88, "N", 4, 29, 4),
    k(89, "M", 4, 33, 4),
    k(90, ",", 4, 37, 4),
    k(91, ".", 4, 41, 4),
    k(92, "/", 4, 45, 4),
    k(94, "Right Shift", 4, 49, 7),
    k(114, "Up / Page Up", 4, 56, 4),
    k(17, "Volume Down", 4, 60, 4),
    k(100, "Left Ctrl", 5, 0, 5),
    k(101, "Fn", 5, 5, 4),
    k(103, "Left Windows", 5, 9, 4),
    k(104, "Left Alt", 5, 13, 4),
    k(111, "Right Alt", 5, 37, 4),
    k(109, "Windows lock", 5, 41, 4),
    k(112, "Right Ctrl", 5, 45, 7),
    k(133, "Left Arrow", 5, 52, 4),
    k(134, "Down / Page Down", 5, 56, 4),
    k(135, "Right Arrow", 5, 60, 4),
    // AWCC lists these, but this keyboard has no such key. Writes still cover them, as AWCC's do.
    absent(33, "JP1"),
    absent(59, "JP3"),
    absent(73, "UK1"),
    absent(82, "JP6"),
    absent(105, "JP4"),
    absent(113, "Page Up"),
    absent(115, "Page Down"),
];

/// One of AWCC's key groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Group {
    pub name: &'static str,
    pub leds: &'static [u8],
}

/// AWCC's four groups, in AWCC's order. W (43) is in both QWER and WASD.
pub const GROUPS: [Group; 4] = [
    Group { name: "Function Keys", leds: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] },
    Group { name: "Numbers", leds: &[21, 22, 23, 24, 25, 26, 27, 28, 29, 30] },
    Group { name: "QWER", leds: &[42, 43, 44, 45] },
    Group { name: "WASD", leds: &[43, 62, 63, 64] },
];

/// The key with this LED id.
pub fn key(id: u8) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.id == id)
}

/// The key with this name, ignoring case and surrounding spaces ("esc", "Left Shift").
pub fn by_name(name: &str) -> Option<&'static KeyDef> {
    let n = name.trim();
    KEYS.iter().find(|k| k.name.eq_ignore_ascii_case(n))
}

/// The group with this name, ignoring case ("wasd", "function keys").
pub fn group(name: &str) -> Option<&'static Group> {
    let n = name.trim();
    GROUPS.iter().find(|g| g.name.eq_ignore_ascii_case(n))
}

/// Every group the key is in, in AWCC's order.
pub fn groups_of(id: u8) -> impl Iterator<Item = &'static Group> {
    GROUPS.iter().filter(move |g| g.leds.contains(&id))
}

/// The group that decides a key's group colour: the last of its groups in AWCC's order (W: WASD).
pub fn group_of(id: u8) -> Option<&'static str> {
    groups_of(id).last().map(|g| g.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// `X17_KEYS` in examples/write_test.rs (the L0 test's list of AWCC's LED ids).
    const X17_KEYS: [u8; 91] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 35, 40, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 55, 59, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 94, 100, 101, 103, 104, 105, 109, 111, 112, 113, 114, 115, 133, 134, 135,
    ];

    #[test]
    fn ninety_one_unique_ids_all_awccs_84_present() {
        let ids: BTreeSet<u8> = KEYS.iter().map(|k| k.id).collect();
        assert_eq!(ids.len(), 91, "unique ids");
        assert_eq!(ids, X17_KEYS.iter().copied().collect::<BTreeSet<_>>(), "exactly AWCC's LED ids");
        let names: BTreeSet<&str> = KEYS.iter().map(|k| k.name).collect();
        assert_eq!(names.len(), 91, "unique names");
        assert!(KEYS.iter().all(|k| k.id <= crate::v5::MAX_KEY_ID));
        assert_eq!(KEYS.iter().filter(|k| k.present).count(), 84, "84 keys on the owner's keyboard");
        let gone: Vec<u8> = KEYS.iter().filter(|k| !k.present).map(|k| k.id).collect();
        assert_eq!(gone, vec![33, 59, 73, 82, 105, 113, 115], "JP1, JP3, UK1, JP6, JP4, Page Up, Page Down");
        assert!(KEYS.iter().filter(|k| !k.present).all(|k| k.row == ABSENT_ROW && k.width == 0));
    }

    #[test]
    fn names_from_awcc_and_the_keycaps() {
        let n = |id| key(id).unwrap().name;
        assert_eq!((n(0), n(1), n(12), n(13), n(14), n(15)), ("ESC", "F1", "F12", "Home", "End", "Delete"));
        assert_eq!((n(16), n(17), n(18), n(19)), ("Speaker Mute", "Volume Down", "Volume Up", "Microphone Mute"));
        assert_eq!((n(20), n(21), n(30), n(31), n(32), n(35)), ("`", "1", "0", "-", "=", "Backspace"));
        assert_eq!((n(40), n(42), n(55), n(61), n(74), n(81), n(94)), ("Tab", "Q", "|", "Caps Lock", "Enter", "Left Shift", "Right Shift"));
        assert_eq!((n(100), n(101), n(103), n(104)), ("Left Ctrl", "Fn", "Left Windows", "Left Alt"));
        assert_eq!((n(109), n(111), n(112)), ("Windows lock", "Right Alt", "Right Ctrl"));
        assert_eq!((n(114), n(133), n(134), n(135)), ("Up / Page Up", "Left Arrow", "Down / Page Down", "Right Arrow"));
        assert_eq!((n(33), n(113), n(115)), ("JP1", "Page Up", "Page Down"), "absent ids keep AWCC's names");
        assert_eq!(by_name("esc").map(|k| k.id), Some(0));
        assert_eq!(by_name(" right ctrl ").map(|k| k.id), Some(112));
        assert_eq!(by_name("nope"), None);
    }

    #[test]
    fn groups_as_awcc_defines_them() {
        let g: Vec<(&str, usize)> = GROUPS.iter().map(|g| (g.name, g.leds.len())).collect();
        assert_eq!(g, vec![("Function Keys", 12), ("Numbers", 10), ("QWER", 4), ("WASD", 4)]);
        let names = |ids: &[u8]| ids.iter().map(|&i| key(i).unwrap().name).collect::<Vec<_>>().join("");
        assert_eq!(names(GROUPS[2].leds), "QWER");
        assert_eq!(names(GROUPS[3].leds), "WASD");
        assert_eq!(names(GROUPS[1].leds), "1234567890");
        assert!(GROUPS[0].leds.iter().enumerate().all(|(i, &id)| key(id).unwrap().name == format!("F{}", i + 1)));
        assert_eq!(group_of(43), Some("WASD"), "W is in QWER and WASD; the later group decides");
        assert_eq!(groups_of(43).count(), 2);
        assert_eq!(group_of(42), Some("QWER"));
        assert_eq!(group_of(0), None);
        assert_eq!(group("function keys").map(|g| g.leds.len()), Some(12));
        assert!(GROUPS.iter().flat_map(|g| g.leds).all(|id| key(*id).is_some_and(|k| k.present)), "every group LED is a key on the keyboard");
    }

    /// Each row, with the space bar's gap on the bottom one, tiles exactly 16 keys (64 quarters).
    #[test]
    fn rows_tile_sixteen_units_without_overlap() {
        for r in 0..ROWS {
            let mut spans: Vec<(u8, u8, &str)> = KEYS.iter().filter(|k| k.present && k.row == r).map(|k| (k.col, k.width, k.name)).collect();
            if r == ROWS - 1 {
                spans.push((SPACE_BAR.0, SPACE_BAR.1, "space bar"));
            }
            spans.sort();
            let mut x = 0;
            for (col, w, name) in &spans {
                assert_eq!(*col, x, "row {r}: {name} starts at {col}, the row is filled to {x}");
                assert!(*w > 0);
                x += w;
            }
            assert_eq!(x, ROW_WIDTH, "row {r}");
        }
        assert!(KEYS.iter().all(|k| !k.present || k.row < ROWS));
        let at = |id| key(id).map(|k| (k.row, k.col, k.width)).unwrap();
        assert_eq!(at(114).1, at(134).1, "the Up arrow sits over the Down arrow");
        assert_eq!((at(35), at(74), at(81), at(112)), ((1, 52, 8), (3, 51, 9), (4, 0, 9), (5, 45, 7)), "Backspace 2, Enter 2.25, Left Shift 2.25, Right Ctrl 1.75");
        assert_eq!(at(133).0, 5, "the arrows are on the bottom row");
    }
}
