//! What Alien Cockpit OSS (the Tauri app) and `cockpit` (the command line) share, so both run one copy:
//!
//! - [`settings`]: the person's choices, their validation, the file both read and write.
//! - [`modes`]: the emergency rule as the app runs it, the mode G-Mode was entered from, raised fans.
//! - [`log`]: the daily log (one JSON-lines file a day) and its cleanup.
//! - [`lighting`], [`games`]: AWCC's lighting database and game library, read-only (Windows).
//! - [`boostkey`]: what a press of the boost key does with the current settings.
//! - [`autostart`]: the sign-in task's arguments.
//! - [`readout`]: alert texts, fan zone names, percent of max.
//!
//! The hardware itself is behind `wiregent_hw::machine::Machine`.

pub mod autostart;
pub mod boostkey;
pub mod games;
pub mod lighting;
pub mod log;
pub mod modes;
pub mod readout;
pub mod settings;

pub use modes::{emergency_step, hottest, record_switch, route_for, Modes, Raised, EMERGENCY_HOLD};
pub use settings::{Effects, Settings, PROFILES};
