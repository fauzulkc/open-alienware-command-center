// The allow-list accepts exactly what the builders produce. Exhaustive over the first bytes.

use alienfx::v4::{self, Action, Effect, PowerSlot};
use alienfx::v5::{self, ColorCount, EffectKind, GlobalEffect, Key};
use alienfx::{allowed, is_memory_write, Api, Rgb, Step};
use std::collections::HashSet;

const RED: Rgb = Rgb::new(0xff, 0, 0);

fn v4_corpus() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut add = |r: &v4::V4Report| out.push(r.as_bytes().to_vec());
    add(&v4::update());
    for s in v4::reset() {
        if let Step::Write(r) = s {
            add(&r);
        }
    }
    add(&v4::set_one_color(1, RED));
    add(&v4::set_multi_color(&(0..26).collect::<Vec<u8>>(), RED).unwrap());
    add(&v4::set_multi_color(&[10, 11], RED).unwrap());
    add(&v4::color_select(1));
    for e in Effect::ALL {
        let a = Action::new(e, 3, 0x40, RED);
        add(&v4::color_set(&[a]).unwrap());
        add(&v4::color_set(&[a, a]).unwrap());
        add(&v4::color_set(&[a, a, a]).unwrap());
    }
    add(&v4::brightness(0, &[1]).unwrap());
    add(&v4::brightness(100, &(0..28).collect::<Vec<u8>>()).unwrap());
    let pw = [Action::new(Effect::Power, 0, 0x10, RED), Action::new(Effect::Power, 0, 0x10, RED)];
    for s in v4::set_power_action(0, &pw).unwrap() {
        if let Step::Write(r) = s {
            add(&r);
        }
    }
    for r in v4::save_lights_state(&[(1, vec![Action::color(RED)])]).unwrap() {
        add(&r);
    }
    out
}

fn v5_corpus() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut add = |r: &v5::V5Report| out.push(r.as_bytes().to_vec());
    add(&v5::reset_report());
    add(&v5::status_request());
    add(&v5::loop_end());
    add(&v5::update());
    add(&v5::color_set(&[Key::new(0, RED)]).unwrap());
    add(&v5::color_set(&(0..15).map(|i| Key::new(i, RED)).collect::<Vec<_>>()).unwrap());
    add(&v5::color_set(&[Key::new(254, Rgb::BLACK)]).unwrap());
    add(&v5::brightness_report(0));
    add(&v5::brightness_report(0xff));
    add(&v5::effect_report(GlobalEffect::Off));
    for kind in EffectKind::ALL {
        for colors in [ColorCount::One, ColorCount::Two, ColorCount::Spectrum] {
            add(&v5::effect_report(GlobalEffect::Effect { kind, tempo: 0x20, colors, color1: RED, color2: RED }));
        }
    }
    out
}

fn prefixes(corpus: &[Vec<u8>]) -> HashSet<[u8; 3]> {
    corpus.iter().map(|r| [r[0], r[1], r[2]]).collect()
}

#[test]
fn every_builder_output_is_allowed_for_its_api_only() {
    for r in v4_corpus() {
        assert!(allowed(&r, Api::V4), "{r:02x?}");
        assert!(!allowed(&r, Api::V5), "{r:02x?}");
    }
    for r in v5_corpus() {
        assert!(allowed(&r, Api::V5), "{r:02x?}");
        assert!(!allowed(&r, Api::V4), "{r:02x?}");
    }
}

#[test]
fn wrong_length_is_refused() {
    for (api, corpus) in [(Api::V4, v4_corpus()), (Api::V5, v5_corpus())] {
        for r in corpus {
            assert!(!allowed(&r[..r.len() - 1], api));
            let mut longer = r.clone();
            longer.push(0);
            assert!(!allowed(&longer, api));
        }
        assert!(!allowed(&[], api));
    }
}

// All 2^24 values of the first three bytes, rest zero. v4 has no zero-tailed command (every one carries a
// non-zero sub-command or count); v5 has exactly reset, status and loop.
#[test]
fn exhaustive_first_three_bytes_zero_tail() {
    let mut a4 = [0u8; 34];
    let mut a5 = [0u8; 64];
    let mut got4 = Vec::new();
    let mut got5 = Vec::new();
    for b0 in 0..=255u8 {
        for b1 in 0..=255u8 {
            for b2 in 0..=255u8 {
                a4[..3].copy_from_slice(&[b0, b1, b2]);
                a5[..3].copy_from_slice(&[b0, b1, b2]);
                if allowed(&a4, Api::V4) {
                    got4.push([b0, b1, b2]);
                }
                if allowed(&a5, Api::V5) {
                    got5.push([b0, b1, b2]);
                }
            }
        }
    }
    assert_eq!(got4, Vec::<[u8; 3]>::new());
    assert_eq!(got5, vec![[0xcc, 0x8c, 0x13], [0xcc, 0x93, 0x00], [0xcc, 0x94, 0x00]]);
}

// For every builder output, every value of (byte 0, byte 1) and every value of byte 2: an accepted mutant
// must carry a report-ID/command/sub-command prefix some builder produces.
#[test]
fn exhaustive_prefix_mutation() {
    for (api, corpus) in [(Api::V4, v4_corpus()), (Api::V5, v5_corpus())] {
        let ok = prefixes(&corpus);
        for r in &corpus {
            let mut m = r.clone();
            for b0 in 0..=255u8 {
                for b1 in 0..=255u8 {
                    m[0] = b0;
                    m[1] = b1;
                    if allowed(&m, api) {
                        assert!(ok.contains(&[m[0], m[1], m[2]]), "{api:?} {m:02x?}");
                    }
                }
            }
            let mut m = r.clone();
            for b2 in 0..=255u8 {
                m[2] = b2;
                if allowed(&m, api) {
                    assert!(ok.contains(&[m[0], m[1], m[2]]), "{api:?} {m:02x?}");
                }
            }
        }
        // Report ID and command byte: only one value each is ever accepted.
        for r in &corpus {
            for b0 in 0..=255u8 {
                let mut m = r.clone();
                m[0] = b0;
                assert_eq!(allowed(&m, api), b0 == r[0], "{api:?} b0={b0:#x}");
            }
        }
    }
}

#[test]
fn shapes_alien_tools_can_send_but_this_crate_cannot_are_refused() {
    let pad4 = |p: &[u8]| {
        let mut a = [0u8; 34];
        a[..p.len()].copy_from_slice(p);
        a
    };
    let pad5 = |p: &[u8]| {
        let mut a = [0u8; 64];
        a[..p.len()].copy_from_slice(p);
        a
    };
    // control 7 (set startup) and control 3 on the stored set: never built.
    assert!(!allowed(&pad4(&[0, 3, 0x21, 0, 7, 0xff, 0xff]), Api::V4));
    assert!(!allowed(&pad4(&[0, 3, 0x21, 0, 3, 0x00, 0x61]), Api::V4));
    // setPower outside 0x5B..=0x60, and setPower 3 (finish and play).
    assert!(!allowed(&pad4(&[0, 3, 0x22, 0, 4, 0, 0x61]), Api::V4));
    assert!(!allowed(&pad4(&[0, 3, 0x22, 0, 4, 0, 0x5a]), Api::V4));
    assert!(!allowed(&pad4(&[0, 3, 0x22, 0, 3, 0, 0x5b]), Api::V4));
    // colorSet with a fourth block at offset 27 (the C++ packing), and a Color block without tempo 0xFA.
    let blk = [0x01, 0x00, 0xdc, 0x00, 0x10, 0xff, 0x00, 0x00];
    let mut four = vec![0, 3, 0x24];
    for _ in 0..4 {
        four.extend(blk);
    }
    assert!(!allowed(&pad4(&four[..34]), Api::V4));
    assert!(!allowed(&pad4(&[0, 3, 0x24, 0x00, 0x00, 0xd0, 0x00, 0x10, 1, 2, 3]), Api::V4));
    // turnOn with inverted level 101, and setOneColor with a count that disagrees with the ids.
    assert!(!allowed(&pad4(&[0, 3, 0x26, 101, 0, 1, 1]), Api::V4));
    assert!(!allowed(&pad4(&[0, 3, 0x27, 1, 2, 3, 0, 1, 5, 6]), Api::V4));
    // v4 opcodes this crate never builds (0x25, 0x20, 0x28).
    for op in [0x20, 0x25, 0x28] {
        assert!(!allowed(&pad4(&[0, 3, op, 0, 1]), Api::V4));
    }
    // v5: effect types 5-7, effect type 1 without 0xFE, colour count byte 3, a gap between key blocks.
    for t in [5u8, 6, 7] {
        assert!(!allowed(&pad5(&[0xcc, 0x80, t, 0x10, 0, 0, 1, 1, 1]), Api::V5));
    }
    assert!(!allowed(&pad5(&[0xcc, 0x80, 0x01, 0x10, 0, 0, 1, 1, 1]), Api::V5));
    assert!(!allowed(&pad5(&[0xcc, 0x80, 0x02, 0x10, 0, 0, 1, 1, 1, 3]), Api::V5));
    assert!(!allowed(&pad5(&[0xcc, 0x8c, 0x02, 0, 1, 1, 1, 1, 0, 0, 0, 0, 3, 1, 1, 1]), Api::V5));
    // v5 opcodes from other API versions (v8 0x0e, v6 0x92, v7 0x40) and v5 mask command 0x79.
    for op in [0x0e, 0x92, 0x40, 0x79] {
        assert!(!allowed(&pad5(&[0xcc, op, 0x02]), Api::V5));
    }
}

#[test]
fn memory_writes_are_flagged() {
    for slot in PowerSlot::ALL {
        for r in v4::power_slot_begin(slot) {
            assert!(is_memory_write(r.as_bytes(), Api::V4));
        }
        assert!(is_memory_write(v4::power_slot_end(slot).as_bytes(), Api::V4));
    }
    for r in v4::save_lights_state(&[]).unwrap() {
        assert!(is_memory_write(r.as_bytes(), Api::V4));
    }
    assert!(!is_memory_write(v4::update().as_bytes(), Api::V4));
    assert!(!is_memory_write(v4::set_one_color(1, RED).as_bytes(), Api::V4));
    for r in v5_corpus() {
        assert!(!is_memory_write(&r, Api::V5));
    }
}
