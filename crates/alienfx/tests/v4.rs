// Byte-exact tests for the API v4 builders. Each expected array was derived by hand from alien-tools
// (MIT, https://github.com/T-Troll/alien-tools): the cited AlienRGB_SDK.cpp function and lines, the COMMV4_*
// templates in alienrgb-controls.h 46-72, and PrepareAndSend's buffer rules (AlienRGB_SDK.cpp 102-110:
// zero-filled `length` bytes, template copied after its length byte, report ID 0 at [0], mods at their offsets).

use alienfx::v4::{self, Action, Effect, PowerSlot, Readiness, Status};
use alienfx::{allowed, is_memory_write, Api, Error, Rgb, Step};

fn pad(prefix: &[u8]) -> [u8; 34] {
    let mut a = [0u8; 34];
    a[..prefix.len()].copy_from_slice(prefix);
    a
}

/// Steps as bytes (writes) or a label (waits/reads) for comparison.
fn steps_bytes(steps: &[v4::V4Step]) -> Vec<Result<[u8; 34], &'static str>> {
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
const RGB: Rgb = Rgb::new(0x12, 0x34, 0x56);

#[test]
fn lengths_are_fixed_at_34() {
    assert_eq!(v4::REPORT_LEN, 34);
    assert_eq!(v4::V4Report::LEN, 34);
    assert_eq!(Api::V4.report_len(), 34);
    assert_eq!(v4::update().as_bytes().len(), 34);
}

// UpdateColors, AlienRGB_SDK.cpp 344: PrepareAndSend(COMMV4_control) with no mods -> the bare template.
#[test]
fn update_colors_cpp_344() {
    assert_eq!(v4::update().as_bytes(), &pad(&[0x00, 0x03, 0x21, 0x00, 0x03, 0xff, 0xff]));
}

// Reset, AlienRGB_SDK.cpp 313-318: WaitForReady, control {4,{4}}, control {4,{1}}.
#[test]
fn reset_cpp_313_318() {
    assert_eq!(
        steps_bytes(&v4::reset()),
        vec![
            Err("wait-ready"),
            Ok(pad(&[0x00, 0x03, 0x21, 0x00, 0x04, 0xff, 0xff])),
            Ok(pad(&[0x00, 0x03, 0x21, 0x00, 0x01, 0xff, 0xff])),
        ]
    );
}

// SetAction AlienFX_A_Color branch, AlienRGB_SDK.cpp 553: COMMV4_setOneColor, {3, {r, g, b, 0, 1, index}}.
#[test]
fn set_one_color_cpp_553_alien_head() {
    let r = v4::set_one_color(1, RGB);
    assert_eq!(r.as_bytes(), &pad(&[0x00, 0x03, 0x27, 0x12, 0x34, 0x56, 0x00, 0x01, 0x01]));
}

// SetMultiColor v4, AlienRGB_SDK.cpp 411-426: ids from offset 8, then {3, {r, g, b, 0, count}}.
#[test]
fn set_multi_color_cpp_407_427_rear_loop() {
    let ids: Vec<u8> = (10..=19).collect();
    let r = v4::set_multi_color(&ids, RED).unwrap();
    let mut e = vec![0x00, 0x03, 0x27, 0xff, 0x00, 0x00, 0x00, 10];
    e.extend(10u8..=19);
    assert_eq!(r.as_bytes(), &pad(&e));
}

// SetMultiColor v4, AlienRGB_SDK.cpp 413: `pos > 33` splits after 26 ids (offsets 8..=33).
#[test]
fn set_multi_color_limits_cpp_413() {
    let ids26: Vec<u8> = (0..26).collect();
    let r = v4::set_multi_color(&ids26, RED).unwrap();
    assert_eq!(r.as_bytes()[7], 26);
    assert_eq!(r.as_bytes()[33], 25, "the 26th id lands on the last byte");
    let ids27: Vec<u8> = (0..27).collect();
    assert_eq!(v4::set_multi_color(&ids27, RED), Err(Error::TooMany { what: "light ids", max: 26, got: 27 }));
    // AlienRGB_SDK.cpp 423: nothing is sent for an empty list; the builder refuses it.
    assert_eq!(v4::set_multi_color(&[], RED), Err(Error::Empty("light ids")));
}

// SetV4Action, AlienRGB_SDK.cpp 507: COMMV4_colorSel with {6, {index}}.
#[test]
fn color_select_cpp_507() {
    assert_eq!(v4::color_select(1).as_bytes(), &pad(&[0x00, 0x03, 0x23, 0x01, 0x00, 0x01, 0x01]));
    assert_eq!(v4::color_select(19).as_bytes(), &pad(&[0x00, 0x03, 0x23, 0x01, 0x00, 0x01, 19]));
}

// SetV4Action block, AlienRGB_SDK.cpp 511-513, opcodes from v4OpCodes (alienrgb-controls.h 72):
// {type < Breathing ? type : Morph, time, v4OpCodes[type], 0, type == Color ? 0xfa : tempo, r, g, b} at offset 3.
#[test]
fn color_set_each_effect_cpp_511_513() {
    let cases: [(Effect, [u8; 3]); 7] = [
        (Effect::Color, [0x00, 0xd0, 0xfa]),
        (Effect::Pulse, [0x01, 0xdc, 0x20]),
        (Effect::Morph, [0x02, 0xcf, 0x20]),
        (Effect::Breathing, [0x02, 0xdc, 0x20]),
        (Effect::Spectrum, [0x02, 0x82, 0x20]),
        (Effect::Rainbow, [0x02, 0xac, 0x20]),
        (Effect::Power, [0x02, 0xe8, 0x20]),
    ];
    for (effect, [ty, op, tempo]) in cases {
        let a = Action::new(effect, 0x07, 0x20, RGB);
        let r = v4::color_set(&[a]).unwrap();
        assert_eq!(
            r.as_bytes(),
            &pad(&[0x00, 0x03, 0x24, ty, 0x07, op, 0x00, tempo, 0x12, 0x34, 0x56]),
            "{effect:?}"
        );
    }
}

// SetV4Action, AlienRGB_SDK.cpp 504-519: colorSel then one colorSet with blocks at 3 and 11.
#[test]
fn v4_action_morph_two_colours_cpp_504_519() {
    let acts = [Action::new(Effect::Morph, 5, 0x30, RED), Action::new(Effect::Morph, 5, 0x30, BLUE)];
    let reps = v4::v4_action(1, &acts).unwrap();
    let bytes: Vec<[u8; 34]> = reps.iter().map(|r| *r.as_bytes()).collect();
    assert_eq!(
        bytes,
        vec![
            pad(&[0x00, 0x03, 0x23, 0x01, 0x00, 0x01, 0x01]),
            pad(&[
                0x00, 0x03, 0x24, //
                0x02, 0x05, 0xcf, 0x00, 0x30, 0xff, 0x00, 0x00, //
                0x02, 0x05, 0xcf, 0x00, 0x30, 0x00, 0x00, 0xff,
            ]),
        ]
    );
}

// SetV4Action, AlienRGB_SDK.cpp 510: three blocks fill offsets 3..=26. A fourth action goes to a second
// colorSet here; alien-tools would place it at 27 and lose its blue byte (offset 34) to the 34-byte length.
#[test]
fn v4_action_three_per_report_cpp_510() {
    let acts: Vec<Action> = (1..=4).map(|i| Action::new(Effect::Pulse, i, 0x10, Rgb::new(i, i, i))).collect();
    let reps = v4::v4_action(2, &acts).unwrap();
    assert_eq!(reps.len(), 3);
    assert_eq!(
        reps[1].as_bytes(),
        &pad(&[
            0x00, 0x03, 0x24, //
            0x01, 0x01, 0xdc, 0x00, 0x10, 0x01, 0x01, 0x01, //
            0x01, 0x02, 0xdc, 0x00, 0x10, 0x02, 0x02, 0x02, //
            0x01, 0x03, 0xdc, 0x00, 0x10, 0x03, 0x03, 0x03,
        ])
    );
    assert_eq!(
        reps[2].as_bytes(),
        &pad(&[0x00, 0x03, 0x24, 0x01, 0x04, 0xdc, 0x00, 0x10, 0x04, 0x04, 0x04])
    );
    assert!(matches!(v4::color_set(&acts), Err(Error::TooMany { max: 3, got: 4, .. })));
    assert_eq!(v4::color_set(&[]), Err(Error::Empty("actions")));
}

// SetAction, AlienRGB_SDK.cpp 522-523 (empty refused) and 551-560 (dispatch on the first action's type).
#[test]
fn set_action_dispatch_cpp_521_560() {
    let c = v4::set_action(1, &[Action::color(RGB), Action::new(Effect::Pulse, 1, 1, RED)]).unwrap();
    assert_eq!(c, vec![v4::set_one_color(1, RGB)], "Color uses setOneColor and only the first action");
    let p = v4::set_action(1, &[Action::new(Effect::Pulse, 1, 2, RED)]).unwrap();
    assert_eq!(p, v4::v4_action(1, &[Action::new(Effect::Pulse, 1, 2, RED)]).unwrap());
    assert_eq!(v4::set_action(0, &[Action::new(Effect::Power, 0, 0, RED)]), Err(Error::PowerAction));
    assert_eq!(v4::set_action(0, &[]), Err(Error::Empty("actions")));
    assert_eq!(v4::set_multi_action(&[]), Err(Error::Empty("lights")));
}

// SetMultiAction default branch, AlienRGB_SDK.cpp 491-496: one SetAction per light.
#[test]
fn set_multi_action_cpp_491_496() {
    let reps = v4::set_multi_action(&[(0, vec![Action::color(RED)]), (1, vec![Action::color(BLUE)])]).unwrap();
    assert_eq!(reps, vec![v4::set_one_color(0, RED), v4::set_one_color(1, BLUE)]);
}

// SetBrightness, AlienRGB_SDK.cpp 738 with brightnessScale[4] = 0x64 (alienrgb-controls.h 15).
#[test]
fn scale_brightness_cpp_738() {
    assert_eq!(v4::scale_brightness(255, 255), 100);
    assert_eq!(v4::scale_brightness(128, 255), 50); // 128 * 100 / 255 = 50.19
    assert_eq!(v4::scale_brightness(255, 128), 50); // (255*128/255 = 128) * 100 / 255
    assert_eq!(v4::scale_brightness(0, 255), 0);
    assert_eq!(v4::scale_brightness(3, 255), 1);
    for b in 0..=255u8 {
        for g in [0u8, 1, 128, 255] {
            assert!(v4::scale_brightness(b, g) <= 100);
        }
    }
}

// SetBrightness v4, AlienRGB_SDK.cpp 747-764: COMMV4_turnOn, ids from 6, {3, {0x64 - bright, 0, count}}.
#[test]
fn brightness_cpp_747_764() {
    let ids: Vec<u8> = [0u8, 1].into_iter().chain(10..=19).collect();
    let r = v4::brightness(50, &ids).unwrap();
    let mut e = vec![0x00, 0x03, 0x26, 0x32, 0x00, 12];
    e.extend(&ids);
    assert_eq!(r.as_bytes(), &pad(&e));
    // Full brightness sends 0, off sends 0x64.
    assert_eq!(v4::brightness(100, &[1]).unwrap().as_bytes(), &pad(&[0x00, 0x03, 0x26, 0x00, 0x00, 0x01, 0x01]));
    assert_eq!(v4::brightness(0, &[1]).unwrap().as_bytes(), &pad(&[0x00, 0x03, 0x26, 0x64, 0x00, 0x01, 0x01]));
}

// SetBrightness v4, AlienRGB_SDK.cpp 751 (`pos > 33`: 28 ids at 6..=33) and 762 (0x64 - bright needs bright <= 100).
#[test]
fn brightness_refuses_bad_input_cpp_751_762() {
    let ids28: Vec<u8> = (0..28).collect();
    assert_eq!(v4::brightness(10, &ids28).unwrap().as_bytes()[33], 27);
    let ids29: Vec<u8> = (0..29).collect();
    assert!(matches!(v4::brightness(10, &ids29), Err(Error::TooMany { max: 28, got: 29, .. })));
    assert!(matches!(v4::brightness(101, &[1]), Err(Error::OutOfRange { value: 101, max: 100, .. })));
    assert_eq!(v4::brightness(10, &[]), Err(Error::Empty("light ids")));
}

const AC: Action = Action::new(Effect::Power, 0, 0x10, Rgb::new(0x00, 0x00, 0xff));
const BATT: Action = Action::new(Effect::Power, 0, 0x10, Rgb::new(0xff, 0xa0, 0x00));
const OFF: Action = Action::new(Effect::Power, 3, 0x64, Rgb::BLACK);

// SetPowerAction, AlienRGB_SDK.cpp 660-685: `tact = *act` then per-slot pushes and front-type changes.
#[test]
fn power_slot_actions_cpp_660_685() {
    let input = [AC, BATT];
    let as_type = |a: Action, e: Effect| Action { effect: e, ..a };
    let cases: [(PowerSlot, Vec<Action>); 6] = [
        (PowerSlot::AcSleep, vec![AC, BATT, AC, OFF]),
        (PowerSlot::AcPower, vec![as_type(AC, Effect::Color), BATT, AC]),
        (PowerSlot::Charge, vec![AC, BATT, AC, BATT]),
        (PowerSlot::BatterySleep, vec![AC, BATT, BATT, OFF]),
        (PowerSlot::BatteryPower, vec![as_type(AC, Effect::Color), BATT, BATT]),
        (PowerSlot::BatteryCritical, vec![as_type(AC, Effect::Pulse), BATT, BATT]),
    ];
    for (slot, expected) in cases {
        assert_eq!(slot.actions(&input).unwrap(), expected, "{slot:?}");
    }
    assert_eq!(PowerSlot::AcSleep.actions(&[]), Err(Error::Empty("power actions")));
}

// One loop iteration of SetPowerAction for cid 0x5C, AlienRGB_SDK.cpp 657-658, 666-668, 686, 688,
// with a single-action input block.
#[test]
fn power_slot_ac_power_bytes_cpp_657_688() {
    let reps = v4::power_slot(PowerSlot::AcPower, 0, &[AC]).unwrap();
    let bytes: Vec<[u8; 34]> = reps.iter().map(|r| *r.as_bytes()).collect();
    assert_eq!(
        bytes,
        vec![
            pad(&[0x00, 0x03, 0x22, 0x00, 0x04, 0x00, 0x5c]),
            pad(&[0x00, 0x03, 0x22, 0x00, 0x01, 0x00, 0x5c]),
            pad(&[0x00, 0x03, 0x23, 0x01, 0x00, 0x01, 0x00]),
            pad(&[
                0x00, 0x03, 0x24, //
                0x00, 0x00, 0xd0, 0x00, 0xfa, 0x00, 0x00, 0xff, // front, retyped to Color
                0x02, 0x00, 0xe8, 0x00, 0x10, 0x00, 0x00, 0xff, // pushed copy, Power -> type 2, op 0xe8
            ]),
            pad(&[0x00, 0x03, 0x22, 0x00, 0x02, 0x00, 0x5c]),
        ]
    );
    assert!(reps.iter().all(|r| allowed(r.as_bytes(), Api::V4)));
}

// SetPowerAction v4, AlienRGB_SDK.cpp 652-698: six slots 0x5B..0x60, control {4,{5}}, WaitForBusy, WaitForReady.
#[test]
fn set_power_action_sequence_cpp_652_698() {
    let steps = v4::set_power_action(0, &[AC, BATT]).unwrap();
    let b = steps_bytes(&steps);
    let n = b.len();
    assert_eq!(b[n - 3], Ok(pad(&[0x00, 0x03, 0x21, 0x00, 0x05, 0xff, 0xff])));
    assert_eq!(b[n - 2], Err("wait-busy"));
    assert_eq!(b[n - 1], Err("wait-ready"));
    // setPower brackets, in slot order.
    let brackets: Vec<(u8, u8)> =
        b.iter().filter_map(|s| s.ok()).filter(|r| r[2] == 0x22).map(|r| (r[4], r[6])).collect();
    let mut expected = Vec::new();
    for cid in 0x5b..0x61u8 {
        expected.extend([(4, cid), (1, cid), (2, cid)]);
    }
    assert_eq!(brackets, expected);
    for s in &steps {
        if let Step::Write(r) = s {
            assert!(allowed(r.as_bytes(), Api::V4), "{r:?}");
        }
    }
    assert_eq!(v4::set_power_action(0, &[]), Err(Error::Empty("power actions")));
}

// SaveLightsState v4, AlienRGB_SDK.cpp 617-625: control {4,0,0x61}, {1,0,0x61}, SetV4Action for non-power
// lights, {2,0,0x61}, {6,0,0x61}.
#[test]
fn save_lights_state_cpp_617_625() {
    let reps = v4::save_lights_state(&[(1, vec![Action::color(RGB)]), (0, vec![AC, BATT])]).unwrap();
    let bytes: Vec<[u8; 34]> = reps.iter().map(|r| *r.as_bytes()).collect();
    assert_eq!(
        bytes,
        vec![
            pad(&[0x00, 0x03, 0x21, 0x00, 0x04, 0x00, 0x61]),
            pad(&[0x00, 0x03, 0x21, 0x00, 0x01, 0x00, 0x61]),
            pad(&[0x00, 0x03, 0x23, 0x01, 0x00, 0x01, 0x01]),
            pad(&[0x00, 0x03, 0x24, 0x00, 0x00, 0xd0, 0x00, 0xfa, 0x12, 0x34, 0x56]),
            pad(&[0x00, 0x03, 0x21, 0x00, 0x02, 0x00, 0x61]),
            pad(&[0x00, 0x03, 0x21, 0x00, 0x06, 0x00, 0x61]),
        ]
    );
    assert!(is_memory_write(&bytes[0], Api::V4));
    assert!(!is_memory_write(v4::update().as_bytes(), Api::V4));
}

// GetDeviceStatus v4, AlienRGB_SDK.cpp 827-830 (buffer[2]); statuses AlienRGB_SDK.h 19-23.
#[test]
fn status_byte_cpp_827_830() {
    assert_eq!(v4::status_byte(&[0x00, 0x00, 33, 0x00]), Some(33));
    assert_eq!(v4::status_byte(&[0x00, 0x00]), None);
    assert_eq!(Status::from_byte(0), Status::None);
    assert_eq!(Status::from_byte(33), Status::Ready);
    assert_eq!(Status::from_byte(34), Status::Busy);
    assert_eq!(Status::from_byte(35), Status::WaitColor);
    assert_eq!(Status::from_byte(36), Status::WaitUpdate);
    assert_eq!(Status::from_byte(38), Status::WasOn);
    assert_eq!(Status::from_byte(37), Status::Other(37));
}

// IsDeviceReady v4, AlienRGB_SDK.cpp 895 (`status ? status != BUSY : 0xff`); WaitForBusy pid patch, 871.
#[test]
fn readiness_cpp_895_and_busy_patch_cpp_871() {
    assert_eq!(v4::readiness(0), Readiness::Stalled);
    assert_eq!(v4::readiness(34), Readiness::NotReady);
    for s in 1..=255u8 {
        if s != 34 {
            assert_eq!(v4::readiness(s), Readiness::Ready, "{s}");
        }
    }
    assert!(v4::waits_for_busy(0x0550), "the x17 R2 controller waits");
    assert!(!v4::waits_for_busy(0x0551));
    assert_eq!((v4::POLL_INTERVAL_MS, v4::BUSY_POLL_LIMIT), (20, 500));
}

// Action enum, AlienRGB_SDK.h 110-118; refuses unknown types.
#[test]
fn effect_try_from_h_110_118() {
    for (i, e) in Effect::ALL.iter().enumerate() {
        assert_eq!(Effect::try_from(i as u8), Ok(*e));
    }
    assert!(Effect::try_from(7).is_err());
    assert_eq!(v4::OP_CODES, [0xd0, 0xdc, 0xcf, 0xdc, 0x82, 0xac, 0xe8]);
}
