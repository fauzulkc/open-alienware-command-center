//! Fixed-length report buffers, colours, errors and multi-report sequences.
#![forbid(unsafe_code)]

use std::fmt;

/// A colour. alien-tools stores colours as `Afx_colorcode` (b, g, r, brightness) but every report it builds
/// writes them in r, g, b order, so that is the only order exposed here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    pub(crate) const fn bytes(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

/// Why a builder refused its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A write would run past the end of the report. Builders only write at fixed offsets, so this means a
    /// builder bug; it is an error rather than a silent truncation.
    Overflow { pos: usize, len: usize, cap: usize },
    /// A list that must not be empty was empty (lights, keys, actions).
    Empty(&'static str),
    /// Too many items for one report.
    TooMany { what: &'static str, max: usize, got: usize },
    /// A value outside the range the protocol accepts.
    OutOfRange { what: &'static str, value: u32, max: u32 },
    /// The action list's first action is `Power`; alien-tools routes that to `SetPowerAction`
    /// (see [`crate::v4::set_power_action`]), which writes the controller's memory.
    PowerAction,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Overflow { pos, len, cap } => {
                write!(f, "write of {len} bytes at offset {pos} exceeds the {cap}-byte report")
            }
            Error::Empty(what) => write!(f, "{what} must not be empty"),
            Error::TooMany { what, max, got } => write!(f, "{got} {what} given, at most {max} fit in one report"),
            Error::OutOfRange { what, value, max } => write!(f, "{what} {value} is out of range (max {max})"),
            Error::PowerAction => write!(f, "power actions go through set_power_action, which writes controller memory"),
        }
    }
}

impl std::error::Error for Error {}

/// One HID report of exactly `N` bytes, report ID first. The length is part of the type, so a report can
/// never be longer (or shorter) than the device's report length.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Report<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Report<N> {
    pub const LEN: usize = N;

    /// Mirrors the buffer setup in alien-tools `Functions::PrepareAndSend` (AlienRGB_SDK.cpp 102-104):
    /// zero the buffer, copy the command template after its length byte, then put the report ID at [0].
    pub(crate) fn from_command(report_id: u8, command: &[u8]) -> Result<Self, Error> {
        let mut r = Report { bytes: [0u8; N] };
        r.put(0, &[report_id])?;
        r.put(1, command)?;
        Ok(r)
    }

    /// Mirrors the `mods` loop in `PrepareAndSend` (AlienRGB_SDK.cpp 106-108): copy `data` at `pos`.
    /// Refuses (instead of truncating) any write that would end past the report.
    pub(crate) fn put(&mut self, pos: usize, data: &[u8]) -> Result<(), Error> {
        let end = pos.checked_add(data.len()).filter(|&e| e <= N);
        match end {
            Some(end) => {
                self.bytes[pos..end].copy_from_slice(data);
                Ok(())
            }
            None => Err(Error::Overflow { pos, len: data.len(), cap: N }),
        }
    }

    pub fn as_bytes(&self) -> &[u8; N] {
        &self.bytes
    }

    pub fn into_bytes(self) -> [u8; N] {
        self.bytes
    }
}

impl<const N: usize> AsRef<[u8]> for Report<N> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl<const N: usize> fmt::Debug for Report<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Report<{N}>[")?;
        for (i, b) in self.bytes.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{b:02x}")?;
        }
        write!(f, "]")
    }
}

/// One step of a multi-report sequence as alien-tools performs it. Only `Write` carries bytes; the other
/// steps are the reads/polls alien-tools does between writes, listed so a later writer can reproduce the
/// timing. This crate never performs any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step<const N: usize> {
    /// Send this report (v4: output report, v5: feature report).
    Write(Report<N>),
    /// v4 `WaitForReady` (AlienRGB_SDK.cpp 853-855): poll the input report until [`crate::v4::readiness`]
    /// is `Ready`, sleeping [`crate::v4::POLL_INTERVAL_MS`] between polls.
    WaitReady,
    /// v4 `WaitForBusy` (AlienRGB_SDK.cpp 870-877): poll until status is BUSY, at most
    /// [`crate::v4::BUSY_POLL_LIMIT`] times; skipped when [`crate::v4::waits_for_busy`] is false.
    WaitBusy,
    /// v5 `GetDeviceStatus` read half (AlienRGB_SDK.cpp 821-823): get feature report 0xCC and read byte 2.
    /// Always follows a `Write` of [`crate::v5::status_request`].
    ReadStatus,
}

impl<const N: usize> Step<N> {
    pub fn report(&self) -> Option<&Report<N>> {
        match self {
            Step::Write(r) => Some(r),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_refuses_write_past_end() {
        let mut r = Report::<4>::from_command(0, &[1, 2, 3]).unwrap();
        assert_eq!(r.as_bytes(), &[0, 1, 2, 3]);
        assert_eq!(r.put(3, &[9, 9]), Err(Error::Overflow { pos: 3, len: 2, cap: 4 }));
        assert_eq!(r.put(usize::MAX, &[9]), Err(Error::Overflow { pos: usize::MAX, len: 1, cap: 4 }));
        assert_eq!(r.as_bytes(), &[0, 1, 2, 3], "a refused write leaves the report unchanged");
        assert!(Report::<3>::from_command(0, &[1, 2, 3]).is_err(), "template longer than the report");
    }

    #[test]
    fn debug_is_hex() {
        let r = Report::<3>::from_command(0xcc, &[0x94]).unwrap();
        assert_eq!(format!("{r:?}"), "Report<3>[cc 94 00]");
    }
}
