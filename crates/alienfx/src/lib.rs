// AlienFX report builders, ported from alien-tools by T-Troll (MIT licence):
//   https://github.com/T-Troll/alien-tools
// The byte layouts, command templates, opcode tables and status values below are taken from that project's
// AlienFX SDK (AlienRGB_SDK.cpp, AlienRGB_SDK.h, alienrgb-controls.h). Copyright (c) T-Troll, MIT licence.
// Tests cite the C++ function and line each expected array was derived from.

//! Pure builders for the AlienFX lighting protocol, API v4 (zone controller, 34-byte output reports) and API
//! v5 (per-key keyboard, 64-byte feature report 0xCC).
//!
//! The builders write to no device. They return fixed-size [`Report`]s (a report can never be
//! longer than its device's report length) or [`Step`] sequences; [`allowed`] re-checks any byte slice
//! against the set of reports this crate can produce, so a writer added later can gate every send on it.
//!
//! The Windows-only [`probe`] module enumerates HID interfaces and reads attributes and capabilities through a
//! query-only handle. [`device`] sends reports: every one passes [`allowed`] and is refused if it is an
//! [`is_memory_write`]; its Windows parts are the only other `unsafe` code. [`x17`] is the x17 R2 keyboard's
//! 91 keys and AWCC's groups. Every other module is `#![forbid(unsafe_code)]`.

#![deny(unsafe_code)]

pub mod allow;
pub mod detect;
#[allow(unsafe_code)]
pub mod device;
pub mod report;
pub mod v4;
pub mod v5;
pub mod x17;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod probe;

pub use allow::{allowed, is_memory_write};
pub use detect::{classify, ApiVersion};
pub use report::{Error, Report, Rgb, Step};

/// The two AlienFX protocol versions this crate builds reports for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Api {
    /// Zone controller (USB 187C:0550 on the x17 R2): 34-byte output reports, report ID 0.
    V4,
    /// Per-key keyboard (USB 0D62:CABC on the x17 R2): 64-byte feature report, report ID 0xCC.
    V5,
}

impl Api {
    /// Report length in bytes, including the report ID byte.
    pub const fn report_len(self) -> usize {
        match self {
            Api::V4 => v4::REPORT_LEN,
            Api::V5 => v5::REPORT_LEN,
        }
    }
}
