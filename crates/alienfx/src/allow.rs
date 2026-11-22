//! The allow-list: accepts exactly the report shapes this crate's builders produce, so a writer added later can
//! re-check every report before it is sent.
#![forbid(unsafe_code)]

use crate::v4::{self, Effect};
use crate::v5::{self, EffectKind};
use crate::Api;

fn zero_from(r: &[u8], from: usize) -> bool {
    r.get(from..).is_some_and(|t| t.iter().all(|&b| b == 0))
}

/// Is `block` an 8-byte v4 colorSet block that [`v4::Action::block`] can produce?
fn v4_block_ok(block: &[u8]) -> bool {
    block.len() == 8
        && block[3] == 0
        && Effect::ALL.iter().any(|e| {
            e.type_byte() == block[0] && e.op_code() == block[2] && (*e != Effect::Color || block[4] == 0xfa)
        })
}

fn allowed_v4(r: &[u8]) -> bool {
    if r.len() != v4::REPORT_LEN || r[0] != v4::REPORT_ID || r[1] != v4::CMD {
        return false;
    }
    match r[2] {
        v4::OP_CONTROL => {
            r[3] == 0
                && matches!((r[4], r[5], r[6]), (1 | 3 | 4 | 5, 0xff, 0xff) | (1 | 2 | 4 | 6, 0x00, 0x61))
                && zero_from(r, 7)
        }
        v4::OP_SET_POWER => {
            r[3] == 0 && matches!(r[4], 1 | 2 | 4) && r[5] == 0 && (0x5b..=0x60).contains(&r[6]) && zero_from(r, 7)
        }
        v4::OP_COLOR_SEL => r[3..6] == [0x01, 0x00, 0x01] && zero_from(r, 7),
        v4::OP_COLOR_SET => {
            // 1..=3 valid blocks at 3, 11, 19; unused blocks and the tail are zero.
            let blocks: Vec<&[u8]> = (0..v4::ACTIONS_PER_COLOR_SET).map(|i| &r[3 + i * 8..3 + i * 8 + 8]).collect();
            let used = blocks.iter().take_while(|b| v4_block_ok(b)).count();
            used >= 1
                && blocks[used..].iter().all(|b| b.iter().all(|&x| x == 0))
                && zero_from(r, 3 + v4::ACTIONS_PER_COLOR_SET * 8)
        }
        v4::OP_TURN_ON => {
            let n = r[5] as usize;
            r[3] <= v4::BRIGHTNESS_SCALE
                && r[4] == 0
                && (1..=v4::MAX_BRIGHTNESS_IDS).contains(&n)
                && zero_from(r, 6 + n)
        }
        v4::OP_SET_ONE_COLOR => {
            let n = r[7] as usize;
            r[6] == 0 && (1..=v4::MAX_COLOR_IDS).contains(&n) && zero_from(r, 8 + n)
        }
        _ => false,
    }
}

fn allowed_v5(r: &[u8]) -> bool {
    if r.len() != v5::REPORT_LEN || r[0] != v5::REPORT_ID {
        return false;
    }
    match r[1] {
        v5::OP_RESET | v5::OP_STATUS => zero_from(r, 2),
        v5::OP_COLOR => match r[2] {
            0x02 => {
                // 1..=15 blocks {id + 1, r, g, b} from offset 4; unused blocks are zero.
                if r[3] != 0 {
                    return false;
                }
                let used = r[4..].chunks(4).take_while(|b| b[0] != 0).count();
                used >= 1 && zero_from(r, 4 + used * 4)
            }
            0x13 => zero_from(r, 3),
            _ => false,
        },
        v5::OP_UPDATE => r[2..4] == [0x01, 0xff] && zero_from(r, 4),
        v5::OP_TURN_ON => r[2..4] == [0x38, 0x9c] && zero_from(r, 5),
        v5::OP_EFFECT => {
            r[4..9] == v5::COMM_SET_EFFECT[3..8]
                && if r[2] == 0x01 {
                    r[3] == 0xfe && zero_from(r, 9)
                } else {
                    EffectKind::try_from(r[2]).is_ok() && r[9] <= 2 && zero_from(r, 16)
                }
        }
        _ => false,
    }
}

/// True only if `report` is byte-for-byte a shape this crate's builders produce for `api`: right length,
/// right report ID, a known command, known sub-commands, counts in range and zero padding.
pub fn allowed(report: &[u8], api: Api) -> bool {
    match api {
        Api::V4 => allowed_v4(report),
        Api::V5 => allowed_v5(report),
    }
}

/// True if `report` is an allowed report that by itself writes the controller's memory: a v4 `setPower`
/// (0x22, the power-state slots 0x5B-0x60) or a v4 control on the stored light set 0x61. The colorSel and
/// colorSet reports sent between such brackets are stored too, but cannot be told apart on their own; a
/// writer has to gate the whole sequence. Needs the owner's go-ahead before sending.
pub fn is_memory_write(report: &[u8], api: Api) -> bool {
    api == Api::V4
        && allowed_v4(report)
        && (report[2] == v4::OP_SET_POWER || (report[2] == v4::OP_CONTROL && report[5..7] == [0x00, 0x61]))
}
