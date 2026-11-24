//! One laptop, whatever talks to it: Dell's AWCC WMI interface on Windows, the Linux kernel's `alienware-wmi`
//! driver (sysfs) on Linux, or the recorded replay (demo and tests). The app, the CLI and the safety rules
//! work through this trait, so the same functions run on both systems.
//!
//! Ids are AWCC's on every backend: profiles 0xA3 Quiet, 0xA0 Balanced, 0xA1 Performance, 0xA4 Full Speed,
//! 0xA5 Battery Saver, 0xAB G-Mode; fans 0x32/0x3B (CPU), 0x33/0x3C (GPU); sensors 0x01 (CPU), 0x06 (GPU).
//! A backend that names things differently (Linux's platform_profile choices, hwmon channels) maps them to
//! these ids and says how in its own docs.
//!
//! Every write keeps the gates of `control`: the verified machine (model + BIOS), only profiles and fans the
//! machine itself reported, boost 0..=100, the person's rate limit (5 a second), and a read-back that says
//! whether the change took.

use crate::control::{BoostChange, ModeChange};
use crate::reader::Snapshot;
use anyhow::Result;
use std::collections::BTreeMap;
use wiregent_oem_alienware::rules::Emergency;

pub trait Machine {
    /// Which backend this is: "awcc-wmi", "linux-sysfs", "replay", "service".
    fn backend(&self) -> &'static str;

    /// Model and BIOS version as the system reports them (for the verified-machine gate and `info`).
    fn identity(&self) -> (String, String);

    /// One reading of every sensor and fan, with the fault-rule alerts that fired on this reading.
    /// `t_ms` is a steady clock in milliseconds (the rules' windows count on it).
    fn snapshot(&mut self, t_ms: u64) -> Snapshot;

    /// Profiles the machine reported it supports (AWCC ids).
    fn profiles(&self) -> Vec<u8>;

    /// Fans the machine reported (AWCC ids).
    fn fans(&self) -> Vec<u8>;

    /// Switches the thermal mode, with every gate; confirmed by reading it back.
    fn set_profile(&mut self, profile: u8, now_ms: u64) -> Result<ModeChange>;

    /// Sets one fan's boost (0 = the firmware's own curve, up to 100), with every gate.
    fn set_fan_boost(&mut self, fan: u8, level: u8, now_ms: u64) -> Result<BoostChange>;

    /// Every fan back to boost 0. Never rate-limited; errors collected, not raised.
    fn reset_fans(&mut self) -> Vec<String>;

    /// Carries out an emergency rule decision (engage: every fan to full; release: each fan back to
    /// `person`'s boost, 0 for the rest). Never rate-limited; gated on the verified machine.
    fn apply_emergency(&mut self, e: Emergency, person: &BTreeMap<u8, u8>) -> Vec<String>;
}
