//! Live lighting in the app: one of the PC-driven animations kept running while the app is open
//! (`lighting.live` in settings.json). The animations themselves are [`crate::clock`],
//! [`crate::mousepaint`] and [`crate::screenmirror`], the same code `cockpit lighting clock|mouse|screen` runs
//! in the foreground; this is their saved choice and options, and one entry point ([`run`]) the app's live
//! thread calls.
//!
//! A run ends when `stop` says so; it then writes the static colours back (AWCC paused around each write
//! only, never for the run).

use crate::clock::{self, ClockBase};
use crate::lightctl::{self, LightIo, LightingSettings, Mode, PowerState, Shown};
use crate::reactive::Clock;
use crate::{mousepaint, screenmirror};
use alienfx::device::Pauser;
use serde::{Deserialize, Serialize};

/// Which animation runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LiveKind {
    /// Nothing: the static colours (or the keyboard's own effect) stay.
    #[default]
    Off,
    /// The keyboard clock: h:mm in big digits, the seconds for 5 s of every 15.
    Clock,
    /// Mouse paint: the screen mapped onto the keys, the pointer leaving a fading trail.
    Mouse,
    /// The screen mirror: one gradient of the screen's colours over the whole keyboard.
    Screen,
}

impl LiveKind {
    pub const ALL: [LiveKind; 4] = [LiveKind::Off, LiveKind::Clock, LiveKind::Mouse, LiveKind::Screen];

    pub fn name(self) -> &'static str {
        match self {
            LiveKind::Off => "off",
            LiveKind::Clock => "clock",
            LiveKind::Mouse => "mouse",
            LiveKind::Screen => "screen",
        }
    }

    pub fn parse(s: &str) -> Option<LiveKind> {
        LiveKind::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// The looks mouse paint accepts ([`mousepaint::look`]).
pub const LOOKS: [&str; 5] = ["glow", "uncover", "reveal", "boost", "screen"];

/// The live animation and its options (`lighting.live`). Defaults are what the owner picked on the keyboard
/// (2026-11-24).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LiveSettings {
    pub kind: LiveKind,
    /// Mouse paint: glow, uncover, reveal, boost or screen.
    pub look: String,
    /// Mouse paint: how long a key takes to fade back, ms (0 keeps the paint).
    pub fade_ms: u64,
    /// Mouse paint, look boost: the dimmed keys' brightness, %.
    pub dim: u32,
    /// Mouse paint: writes a second at most.
    pub mouse_fps: u32,
    /// Screen mirror: writes a second at most.
    pub screen_fps: u32,
    /// Screen mirror: how slowly the keys follow the screen, ms.
    pub smooth_ms: u64,
    /// Screen mirror: how much the screen's areas keep their own colours, 0-1.
    pub spread: f64,
    /// Screen mirror: how much deeper the colours are than the screen's averages, 1-4.
    pub saturation: f64,
    /// Screen mirror: read the screen on the graphics card (least processor, more memory) or through GDI.
    pub gpu: bool,
}

impl Default for LiveSettings {
    fn default() -> Self {
        LiveSettings {
            kind: LiveKind::Off,
            look: "glow".into(),
            fade_ms: mousepaint::DEFAULT_FADE_MS,
            dim: mousepaint::DEFAULT_DIM_PCT,
            mouse_fps: 10,
            screen_fps: screenmirror::DEFAULT_FPS,
            smooth_ms: screenmirror::DEFAULT_SMOOTH_MS,
            spread: screenmirror::DEFAULT_SPREAD,
            saturation: screenmirror::DEFAULT_SATURATION,
            gpu: true,
        }
    }
}

impl LiveSettings {
    pub fn validate(&self) -> Result<(), String> {
        if mousepaint::look(&self.look).is_none() {
            return Err(format!("live: '{}' isn't a mouse paint look: use {}", self.look, LOOKS.join(", ")));
        }
        if !mousepaint::FADE_RANGE.contains(&self.fade_ms) {
            return Err(format!("live: a fade of {} ms is out of range: 0-10000", self.fade_ms));
        }
        if !(1..=100).contains(&self.dim) {
            return Err(format!("live: a dimmed base of {} % is out of range: 1-100", self.dim));
        }
        if !mousepaint::FPS_RANGE.contains(&self.mouse_fps) || !screenmirror::FPS_RANGE.contains(&self.screen_fps) {
            return Err("live: writes a second are out of range: 1-30".into());
        }
        if !screenmirror::SMOOTH_RANGE.contains(&self.smooth_ms) {
            return Err(format!("live: an ease of {} ms is out of range: 0-10000", self.smooth_ms));
        }
        if !(self.spread.is_finite() && (0.0..=1.0).contains(&self.spread)) {
            return Err(format!("live: spread {} is out of range: 0-1", self.spread));
        }
        if !(self.saturation.is_finite() && screenmirror::SATURATION_RANGE.contains(&self.saturation)) {
            return Err(format!("live: saturation {} is out of range: 1-4", self.saturation));
        }
        Ok(())
    }

    /// The screen mirror's options.
    pub fn ambient(&self) -> screenmirror::Ambient {
        screenmirror::Ambient { fps: self.screen_fps, smooth_ms: self.smooth_ms, spread: self.spread, saturation: self.saturation }
    }
}

/// The live animation should run: one is chosen, the lighting is custom, the keyboard is lit, and the
/// (turned-down) reactive glow isn't on.
pub fn wanted(s: &LightingSettings, p: &PowerState) -> bool {
    s.live.kind != LiveKind::Off && s.mode == Mode::Custom && !s.reactive.on && lightctl::level(s, p) > 0
}

/// Runs the chosen animation on `io` until `stop(now)`, then writes the static colours back. Returns what it
/// did, as text. Windows only (it reads the clock, the pointer or the screen through Windows).
#[allow(unused_variables)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, clock: &mut dyn Clock, stop: &mut dyn FnMut(u64) -> bool) -> Result<String, String> {
    s.live.validate()?;
    #[cfg(windows)]
    {
        let l = &s.live;
        match l.kind {
            LiveKind::Off => Ok("no live animation chosen".into()),
            LiveKind::Clock => {
                let sum = clock::run(io, s, p, prev, pauser, ClockBase::Off, clock, &mut clock::local_now, stop)?;
                Ok(format!("The clock ran {:.0} s ({} changes); the static colours are back.", sum.ran_ms as f64 / 1_000.0, sum.frames))
            }
            LiveKind::Mouse => {
                let (base, brush) = mousepaint::look(&l.look).ok_or("unknown look")?;
                let mut src = mousepaint::win::WinPointer::new();
                let sum = mousepaint::run(io, s, p, prev, pauser, base, brush, l.dim, l.mouse_fps, l.fade_ms, clock, &mut src, stop)?;
                Ok(format!("Mouse paint ran {:.0} s ({} writes); the static colours are back.", sum.ran_ms as f64 / 1_000.0, sum.frames))
            }
            LiveKind::Screen => {
                let mut src = screenmirror::open(l.gpu)?;
                let sum = screenmirror::run(io, s, p, prev, pauser, &l.ambient(), clock, src.as_mut(), stop)?;
                Ok(format!("The screen mirror ran {:.0} s ({} writes); the static colours are back.", sum.ran_ms as f64 / 1_000.0, sum.frames))
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = ClockBase::Off;
        Err(lightctl::WINDOWS_ONLY.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_owners_picks_and_valid() {
        let l = LiveSettings::default();
        assert_eq!((l.kind, l.look.as_str(), l.dim, l.spread, l.saturation, l.gpu), (LiveKind::Off, "glow", 70, 1.0, 2.0, true));
        l.validate().unwrap();
        // Older settings files have no `live`: off.
        let s: LightingSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.live, LiveSettings::default());
        let s: LightingSettings = serde_json::from_str(r#"{"live":{"kind":"screen","spread":0.5}}"#).unwrap();
        assert_eq!((s.live.kind, s.live.spread, s.live.look.as_str()), (LiveKind::Screen, 0.5, "glow"));
    }

    #[test]
    fn bad_options_are_refused() {
        let bad = |f: &dyn Fn(&mut LiveSettings)| {
            let mut l = LiveSettings::default();
            f(&mut l);
            l.validate().is_err()
        };
        assert!(bad(&|l| l.look = "sparkle".into()));
        assert!(bad(&|l| l.fade_ms = 20_000));
        assert!(bad(&|l| l.dim = 0));
        assert!(bad(&|l| l.mouse_fps = 0));
        assert!(bad(&|l| l.screen_fps = 31));
        assert!(bad(&|l| l.spread = 1.5));
        assert!(bad(&|l| l.saturation = 0.5));
        assert!(bad(&|l| l.smooth_ms = 60_000));
        let mut s = LightingSettings::default();
        s.live.look = "sparkle".into();
        assert!(s.validate().is_err(), "the lighting settings check the live options too");
    }

    #[test]
    fn it_runs_only_when_it_can_show() {
        let p = PowerState::default();
        let mut s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        assert!(!wanted(&s, &p), "off");
        s.live.kind = LiveKind::Clock;
        assert!(wanted(&s, &p));
        s.mode = Mode::Awcc;
        assert!(!wanted(&s, &p), "AWCC's lighting");
        s.mode = Mode::Custom;
        s.keyboard.on = false;
        assert!(!wanted(&s, &p), "keyboard off");
        s.keyboard.on = true;
        s.reactive.on = true;
        assert!(!wanted(&s, &p), "never with the reactive glow");
        assert_eq!(LiveKind::parse("Screen"), Some(LiveKind::Screen));
        assert_eq!(LiveKind::parse("x"), None);
    }
}
