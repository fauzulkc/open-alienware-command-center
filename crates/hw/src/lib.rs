//! The read side of the `wiregent.alienware` plugin (H1 of docs/plans/plugins-v1.md).
//!
//! - [`reader`]: discovery once, then snapshots with checked values and the fault rules.
//! - [`health`]: battery wear, disk wear, crash and hardware-error events.
//! - [`drift`]: Dell's services against the owner's rules.
//!
//! Writes: only thermal-mode switches, through `control::set_profile` and its gates. Nothing here changes a
//! service.

#[cfg(windows)]
pub mod client;
pub mod client_identity;
pub mod control;
pub mod drift;
pub mod clocks;
pub mod health;
pub mod linux;
pub mod machine;
pub mod machines;
pub mod proto;
pub mod reader;
pub mod replay;
#[cfg(windows)]
pub mod power;
#[cfg(windows)]
pub mod wmi;

use wiregent_oem_alienware::Call;

/// One firmware read. Real: [`wmi::Awcc`]. Tests and demo: [`replay::Replay`].
pub trait Firmware {
    fn read(&self, c: Call) -> anyhow::Result<u32>;
}

impl<T: Firmware + ?Sized> Firmware for Box<T> {
    fn read(&self, c: Call) -> anyhow::Result<u32> {
        (**self).read(c)
    }
}

#[cfg(windows)]
impl Firmware for wmi::Awcc {
    fn read(&self, c: Call) -> anyhow::Result<u32> {
        wmi::Awcc::read(self, c)
    }
}

/// Whether this process runs elevated (the firmware's WMI class needs it).
#[cfg(windows)]
pub fn is_admin() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

#[cfg(not(windows))]
pub fn is_admin() -> bool {
    false
}
