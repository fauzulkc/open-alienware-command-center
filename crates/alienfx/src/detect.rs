//! Which AlienFX API a HID interface speaks, by alien-tools' rules (`Functions::AlienFXProbeDevice`,
//! AlienRGB_SDK.cpp 194-235). Pure: takes the numbers a probe reads, touches no device.
#![forbid(unsafe_code)]

use crate::Api;

pub const VID_ALIENWARE: u16 = 0x187c;
pub const VID_DARFON: u16 = 0x0d62;
pub const VID_MICROCHIP: u16 = 0x0424;
pub const VID_PRIMAX: u16 = 0x0461;
pub const VID_CHICONY: u16 = 0x04f2;

/// `Afx_Version` (AlienRGB_SDK.h 97-108), the USB ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApiVersion {
    V2,
    V3,
    V4,
    V5,
    V6,
    V7,
    V8,
}

impl ApiVersion {
    pub const fn number(self) -> u8 {
        match self {
            ApiVersion::V2 => 2,
            ApiVersion::V3 => 3,
            ApiVersion::V4 => 4,
            ApiVersion::V5 => 5,
            ApiVersion::V6 => 6,
            ApiVersion::V7 => 7,
            ApiVersion::V8 => 8,
        }
    }

    /// The protocol this crate builds reports for, if any.
    pub const fn api(self) -> Option<Api> {
        match self {
            ApiVersion::V4 => Some(Api::V4),
            ApiVersion::V5 => Some(Api::V5),
            _ => None,
        }
    }
}

/// The classification and the report length alien-tools would use (`length`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detected {
    pub version: ApiVersion,
    pub report_len: u16,
}

/// Mirrors AlienRGB_SDK.cpp 194-235. `usage` is `HIDP_CAPS::Usage` (alien-tools does not check the usage
/// page), the lengths are `HIDP_CAPS::{Output,Feature}ReportByteLength`.
///
/// - Darfon 0D62: usage 0xCC with no output report is v5, length = feature length.
/// - Alienware 187C: output length 9 / 12 / 34 / 65 is v2 / v3 / v4 / v6, length = output length.
/// - Other vendors with output length 65: Microchip 0424 (except PID 0x274C) v6, Primax 0461 v7, Chicony
///   04F2 v8.
pub fn classify(vid: u16, pid: u16, usage: u16, output_len: u16, feature_len: u16) -> Option<Detected> {
    let (version, report_len) = match vid {
        VID_DARFON => {
            if usage == 0xcc && output_len == 0 {
                (ApiVersion::V5, feature_len)
            } else {
                return None;
            }
        }
        VID_ALIENWARE => match output_len {
            9 => (ApiVersion::V2, output_len),
            12 => (ApiVersion::V3, output_len),
            34 => (ApiVersion::V4, output_len),
            65 => (ApiVersion::V6, output_len),
            _ => return None,
        },
        _ if output_len == 65 => match vid {
            VID_MICROCHIP if pid != 0x274c => (ApiVersion::V6, output_len),
            VID_PRIMAX => (ApiVersion::V7, output_len),
            VID_CHICONY => (ApiVersion::V8, output_len),
            _ => return None,
        },
        _ => return None,
    };
    Some(Detected { version, report_len })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(vid: u16, pid: u16, usage: u16, out: u16, feat: u16) -> Option<(ApiVersion, u16)> {
        classify(vid, pid, usage, out, feat).map(|d| (d.version, d.report_len))
    }

    // AlienRGB_SDK.cpp 197-201: Darfon usage 0xCC, no output report -> v5 with the feature length.
    #[test]
    fn x17_keyboard_col02_is_v5() {
        assert_eq!(v(0x0d62, 0xcabc, 0xcc, 0, 64), Some((ApiVersion::V5, 64)));
        assert_eq!(ApiVersion::V5.api(), Some(Api::V5));
    }

    // AlienRGB_SDK.cpp 198: other Darfon collections (COL01 usage 0x10, keyboard, consumer) are not v5.
    #[test]
    fn x17_keyboard_other_collections_are_not_lighting() {
        assert_eq!(v(0x0d62, 0xcabc, 0x10, 0, 64), None);
        assert_eq!(v(0x0d62, 0xcabc, 0x06, 0, 0), None);
        assert_eq!(v(0x0d62, 0xcabc, 0xcc, 8, 64), None, "0xCC with an output report is not v5");
    }

    // AlienRGB_SDK.cpp 203-213: Alienware by output length.
    #[test]
    fn alienware_by_output_length() {
        assert_eq!(v(0x187c, 0x0550, 1, 34, 0), Some((ApiVersion::V4, 34)));
        assert_eq!(v(0x187c, 0x0550, 1, 9, 0), Some((ApiVersion::V2, 9)));
        assert_eq!(v(0x187c, 0x0550, 1, 12, 0), Some((ApiVersion::V3, 12)));
        assert_eq!(v(0x187c, 0x0550, 1, 65, 0), Some((ApiVersion::V6, 65)));
        assert_eq!(v(0x187c, 0x0550, 1, 33, 0), None);
        assert_eq!(v(0x187c, 0x0550, 1, 0, 0), None);
        assert_eq!(ApiVersion::V4.api(), Some(Api::V4));
        assert_eq!(ApiVersion::V6.api(), None);
    }

    // AlienRGB_SDK.cpp 221-234: other vendors need output length 65.
    #[test]
    fn other_vendors() {
        assert_eq!(v(0x0424, 0x1234, 0, 65, 0), Some((ApiVersion::V6, 65)));
        assert_eq!(v(0x0424, 0x274c, 0, 65, 0), None);
        assert_eq!(v(0x0461, 1, 0, 65, 0), Some((ApiVersion::V7, 65)));
        assert_eq!(v(0x04f2, 1, 0, 65, 0), Some((ApiVersion::V8, 65)));
        assert_eq!(v(0x04f2, 1, 0, 64, 0), None);
        assert_eq!(v(0x1234, 1, 0, 65, 0), None);
    }
}
