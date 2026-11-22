// Byte-exact tests for the API v5 builders. Each expected array was derived by hand from alien-tools
// (MIT, https://github.com/T-Troll/alien-tools): the cited AlienRGB_SDK.cpp function and lines, the COMMV5_*
// templates in alienrgb-controls.h 76-86, and PrepareAndSend's buffer rules (AlienRGB_SDK.cpp 102-110:
// zero-filled `length` bytes, template copied after its length byte, report ID 0xCC at [0], mods at offsets).

use alienfx::v5::{self, ColorCount, EffectKind, GlobalEffect, Key};
use alienfx::{allowed, Api, Error, Rgb, Step};

fn pad(prefix: &[u8]) -> [u8; 64] {
    let mut a = [0u8; 64];
    a[..prefix.len()].copy_from_slice(prefix);
    a
}

fn steps_bytes(steps: &[v5::V5Step]) -> Vec<Result<[u8; 64], &'static str>> {
    steps
        .iter()
        .map(|s| match s {
            Step::Write(r) => Ok(*r.as_bytes()),
            Step::WaitReady => Err("wait-ready"),
            Step::WaitBusy => Err("wait-busy"),
            Step::ReadStatus => Err("read-status"),
        })
        .collect()
}

const RED: Rgb = Rgb::new(0xff, 0x00, 0x00);
const BLUE: Rgb = Rgb::new(0x00, 0x00, 0xff);

const RESET: [u8; 2] = [0xcc, 0x94];
const STATUS: [u8; 2] = [0xcc, 0x93];
const LOOP: [u8; 3] = [0xcc, 0x8c, 0x13];
const UPDATE: [u8; 4] = [0xcc, 0x8b, 0x01, 0xff];

#[test]
fn lengths_are_fixed_at_64() {
    assert_eq!(v5::REPORT_LEN, 64);
    assert_eq!(v5::V5Report::LEN, 64);
    assert_eq!(Api::V5.report_len(), 64);
    assert_eq!(v5::KEYS_PER_REPORT, 15);
}

// COMMV5_reset / COMMV5_status (alienrgb-controls.h 76-77); Reset v5, AlienRGB_SDK.cpp 308-312
// (reset, then GetDeviceStatus 820-823: status request + get feature).
#[test]
fn reset_cpp_308_312() {
    assert_eq!(v5::reset_report().as_bytes(), &pad(&RESET));
    assert_eq!(v5::status_request().as_bytes(), &pad(&STATUS));
    assert_eq!(steps_bytes(&v5::reset()), vec![Ok(pad(&RESET)), Ok(pad(&STATUS)), Err("read-status")]);
}

// COMMV5_loop (alienrgb-controls.h 80) and COMMV5_update (81); UpdateColors v5, AlienRGB_SDK.cpp 340.
#[test]
fn loop_and_update_cpp_340() {
    assert_eq!(v5::loop_end().as_bytes(), &pad(&LOOP));
    assert_eq!(v5::update().as_bytes(), &pad(&UPDATE));
}

// AddV5DataBlock, AlienRGB_SDK.cpp 371-373 ({index + 1, r, g, b}), packed from offset 4 by SetMultiAction
// (480-488) into COMMV5_colorSet {0x8c, 0x02}.
#[test]
fn color_set_cpp_371_373_480_488() {
    let r = v5::color_set(&[Key::new(0, Rgb::new(1, 2, 3)), Key::new(5, Rgb::new(4, 5, 6))]).unwrap();
    assert_eq!(r.as_bytes(), &pad(&[0xcc, 0x8c, 0x02, 0x00, 0x01, 0x01, 0x02, 0x03, 0x06, 0x04, 0x05, 0x06]));
}

// SetMultiAction v5, AlienRGB_SDK.cpp 483 (`bPos < length`, length 64): blocks at 4..=60, 15 per report.
#[test]
fn color_set_limits_cpp_483() {
    let keys: Vec<Key> = (0..15).map(|i| Key::new(i, Rgb::new(i, i, i))).collect();
    let r = v5::color_set(&keys).unwrap();
    assert_eq!(&r.as_bytes()[60..64], &[15, 14, 14, 14], "the 15th block ends on the last byte");
    let keys16: Vec<Key> = (0..16).map(|i| Key::new(i, RED)).collect();
    assert!(matches!(v5::color_set(&keys16), Err(Error::TooMany { max: 15, got: 16, .. })));
    assert_eq!(v5::color_set(&[]), Err(Error::Empty("keys")));
    // AlienRGB_SDK.cpp 372: index + 1 in a byte; 254 -> 0xFF, 255 would wrap to 0 and is refused.
    assert_eq!(v5::color_set(&[Key::new(254, RED)]).unwrap().as_bytes()[4], 0xff);
    assert!(matches!(v5::color_set(&[Key::new(255, RED)]), Err(Error::OutOfRange { value: 255, max: 254, .. })));
}

// SetAction v5, AlienRGB_SDK.cpp 545-548: one block at 4, then COMMV5_loop.
#[test]
fn set_key_cpp_545_548() {
    let reps = v5::set_key(9, RED).unwrap();
    let bytes: Vec<[u8; 64]> = reps.iter().map(|r| *r.as_bytes()).collect();
    assert_eq!(bytes, vec![pad(&[0xcc, 0x8c, 0x02, 0x00, 0x0a, 0xff, 0x00, 0x00]), pad(&LOOP)]);
}

// SetMultiColor v5, AlienRGB_SDK.cpp 396-406: blocks for each id with the same colour, then loop.
#[test]
fn set_multi_color_cpp_396_406() {
    let reps = v5::set_multi_color(&[1, 2], BLUE).unwrap();
    let bytes: Vec<[u8; 64]> = reps.iter().map(|r| *r.as_bytes()).collect();
    assert_eq!(
        bytes,
        vec![pad(&[0xcc, 0x8c, 0x02, 0x00, 0x02, 0x00, 0x00, 0xff, 0x03, 0x00, 0x00, 0xff]), pad(&LOOP)]
    );
}

// SetMultiAction v5, AlienRGB_SDK.cpp 482-489: more than 15 keys -> another colorSet, then one loop.
#[test]
fn set_keys_splits_cpp_482_489() {
    let keys: Vec<Key> = (0..16).map(|i| Key::new(i, RED)).collect();
    let reps = v5::set_keys(&keys).unwrap();
    assert_eq!(reps.len(), 3);
    assert_eq!(reps[1].as_bytes(), &pad(&[0xcc, 0x8c, 0x02, 0x00, 16, 0xff, 0x00, 0x00]));
    assert_eq!(reps[2].as_bytes(), &pad(&LOOP));
    assert_eq!(v5::set_keys(&[]), Err(Error::Empty("keys")));
}

// The apply sequence: Reset (308-312), colour blocks + loop (545-548), UpdateColors (340).
#[test]
fn apply_keys_sequence_cpp_308_340_548() {
    let steps = v5::apply_keys(&[Key::new(0, RED)]).unwrap();
    assert_eq!(
        steps_bytes(&steps),
        vec![
            Ok(pad(&RESET)),
            Ok(pad(&STATUS)),
            Err("read-status"),
            Ok(pad(&[0xcc, 0x8c, 0x02, 0x00, 0x01, 0xff, 0x00, 0x00])),
            Ok(pad(&LOOP)),
            Ok(pad(&UPDATE)),
        ]
    );
}

// SetBrightness, AlienRGB_SDK.cpp 738 with brightnessScale[5] = 0xff (alienrgb-controls.h 15).
#[test]
fn scale_brightness_cpp_738() {
    assert_eq!(v5::scale_brightness(255, 255), 255);
    assert_eq!(v5::scale_brightness(128, 255), 128);
    assert_eq!(v5::scale_brightness(255, 128), 128);
    assert_eq!(v5::scale_brightness(100, 100), 39); // 10000 / 255 = 39
    assert_eq!(v5::scale_brightness(0, 255), 0);
}

// SetBrightness v5, AlienRGB_SDK.cpp 744-745: Reset, then COMMV5_turnOnSet {0x83, 0x38, 0x9c} with {4, {bright}}.
#[test]
fn brightness_cpp_744_745() {
    assert_eq!(v5::brightness_report(0x80).as_bytes(), &pad(&[0xcc, 0x83, 0x38, 0x9c, 0x80]));
    assert_eq!(
        steps_bytes(&v5::brightness(0xff)),
        vec![Ok(pad(&RESET)), Ok(pad(&STATUS)), Err("read-status"), Ok(pad(&[0xcc, 0x83, 0x38, 0x9c, 0xff]))]
    );
}

// SetGlobalEffects v5, AlienRGB_SDK.cpp 799-800: effType 0 -> COMMV5_setEffect with {2, {1, 0xfe}}.
#[test]
fn global_effect_off_cpp_799_800() {
    assert_eq!(
        v5::effect_report(GlobalEffect::Off).as_bytes(),
        &pad(&[0xcc, 0x80, 0x01, 0xfe, 0x00, 0x00, 0x01, 0x01, 0x01])
    );
}

// SetGlobalEffects v5, AlienRGB_SDK.cpp 802-803: {2, {effType, tempo}}, {9, {nc - 1, r1, g1, b1, r2, g2, b2}}
// over COMMV5_setEffect {0x80, 0x02, 0x07, 0x00, 0x00, 0x01, 0x01, 0x01} (alienrgb-controls.h 83).
#[test]
fn global_effect_bytes_cpp_802_803() {
    let e = GlobalEffect::Effect {
        kind: EffectKind::Breathing,
        tempo: 0x10,
        colors: ColorCount::One,
        color1: RED,
        color2: Rgb::BLACK,
    };
    assert_eq!(
        v5::effect_report(e).as_bytes(),
        &pad(&[0xcc, 0x80, 0x02, 0x10, 0x00, 0x00, 0x01, 0x01, 0x01, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00])
    );
    let e = GlobalEffect::Effect {
        kind: EffectKind::DualColorWave,
        tempo: 0x40,
        colors: ColorCount::Two,
        color1: RED,
        color2: BLUE,
    };
    assert_eq!(
        v5::effect_report(e).as_bytes(),
        &pad(&[0xcc, 0x80, 0x04, 0x40, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff])
    );
    let e = GlobalEffect::Effect {
        kind: EffectKind::SingleColorWave,
        tempo: 0x05,
        colors: ColorCount::Spectrum,
        color1: Rgb::new(1, 2, 3),
        color2: Rgb::new(4, 5, 6),
    };
    assert_eq!(
        v5::effect_report(e).as_bytes(),
        &pad(&[0xcc, 0x80, 0x03, 0x05, 0x00, 0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
    );
}

// Effect type byte for each kind, alienrgb-controls.h 85.
#[test]
fn effect_kind_bytes_h_85() {
    let expected = [
        (EffectKind::Breathing, 0x02),
        (EffectKind::SingleColorWave, 0x03),
        (EffectKind::DualColorWave, 0x04),
        (EffectKind::Pulse, 0x08),
        (EffectKind::MixPulse, 0x09),
        (EffectKind::NightRider, 0x0a),
        (EffectKind::Laser, 0x0b),
    ];
    for (k, b) in expected {
        let r = v5::effect_report(GlobalEffect::Effect {
            kind: k,
            tempo: 0,
            colors: ColorCount::One,
            color1: Rgb::BLACK,
            color2: Rgb::BLACK,
        });
        assert_eq!(r.as_bytes()[2], b, "{k:?}");
        assert_eq!(EffectKind::try_from(b), Ok(k));
    }
}

// Preconditions: types 0/1 are GlobalEffect::Off, 5-7 are "off?" (alienrgb-controls.h 85), nc - 1 needs
// nc >= 1 (AlienRGB_SDK.cpp 802) and nc 3 is spectrum (AlienRGB_SDK.h 238).
#[test]
fn effect_preconditions_refused() {
    for b in [0x00, 0x01, 0x05, 0x06, 0x07, 0x0c, 0xff] {
        assert!(EffectKind::try_from(b).is_err(), "{b:#x}");
    }
    assert!(ColorCount::try_from(0).is_err());
    assert!(ColorCount::try_from(4).is_err());
    assert_eq!(ColorCount::try_from(3), Ok(ColorCount::Spectrum));
}

// SetGlobalEffects v5 sequence, AlienRGB_SDK.cpp 798-804: Reset, setEffect, UpdateColors.
#[test]
fn global_effect_sequence_cpp_798_804() {
    let steps = v5::global_effect(GlobalEffect::Off);
    assert_eq!(
        steps_bytes(&steps),
        vec![
            Ok(pad(&RESET)),
            Ok(pad(&STATUS)),
            Err("read-status"),
            Ok(pad(&[0xcc, 0x80, 0x01, 0xfe, 0x00, 0x00, 0x01, 0x01, 0x01])),
            Ok(pad(&UPDATE)),
        ]
    );
    for s in &steps {
        if let Step::Write(r) = s {
            assert!(allowed(r.as_bytes(), Api::V5));
        }
    }
}

// GetDeviceStatus v5, AlienRGB_SDK.cpp 821-823 (buffer[2]); IsDeviceReady v5, 887 (!= WAITUPDATE 0x80).
#[test]
fn status_cpp_821_823_887() {
    assert_eq!(v5::status_byte(&[0xcc, 0x93, 0x8c]), Some(0x8c));
    assert_eq!(v5::status_byte(&[0xcc]), None);
    assert!(!v5::is_ready(0x80));
    assert!(v5::is_ready(v5::STATUS_START_COMMAND));
    assert!(v5::is_ready(v5::STATUS_IN_COMMAND));
    assert!(v5::is_ready(0));
}
