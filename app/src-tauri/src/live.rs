//! Live lighting in the app (docs/lighting-control.md, "Live lighting in the app"): a base (the static
//! colours, the clock, mouse paint or the screen mirror) with a sweep and a mouse trail on top
//! (`lighting.live`), and the clock while the display is off (`clockOnDisplayOff`), kept running on this
//! thread while the app is open, tray included. The layers are cockpit-core's `live`.
//!
//! - It runs only while there is something to show: custom lighting, a lit keyboard (not dark for sleep, lid
//!   or display), and a layer chosen or the display off with the clock set ([`live::plan`]).
//! - Every other lighting write of the app (a save, a preview, a power change, AWCC's look, quitting) first
//!   takes a hold on it ([`lights::LIVE`]): the animation stops with the static colours written back, the
//!   write happens, and once nothing holds it this thread starts it again from the saved settings.
//! - It also stops on its own when the saved settings change or the plan does (the display turning off or
//!   on, the keyboard going dark), checked twice a second, and starts again with the new plan.
//! - AWCC's lighting programs are paused around each of its writes only, never for the run (holding them
//!   makes typing slow).
//! - A run that fails is shown as the last apply and isn't tried again for 30 s unless the settings change.

use crate::glow::Keys;
use crate::lights::{self, Shared, LIVE};
use cockpit_core::lightctl::{self, alienfx::device::Pauser, LastApply, LightingSettings, PowerState, Shown, SystemIo};
use cockpit_core::live;
use cockpit_core::reactive::Clock;
use std::time::{Duration, Instant};

/// The wait between checks while nothing runs, ms.
const IDLE_MS: u64 = 250;
/// While it runs, the saved settings and power state are compared this often, ms.
const CHECK_MS: u64 = 500;
/// A failed run isn't tried again with the same settings for this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// A steady clock that sleeps for real.
struct Steady(Instant);

impl Clock for Steady {
    fn now_ms(&mut self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// The live thread: forever, start the chosen animation when it should run, and run it until it should stop.
/// `keys`/`rx`: key-downs from the app's read-only Raw Input listener, delivered only while a run uses the
/// key-press laser.
pub fn run(shared: Shared, keys: std::sync::Arc<Keys>, rx: std::sync::mpsc::Receiver<u8>, settings: impl Fn() -> LightingSettings) {
    // Let the app's first write (its look put back at start) go first.
    std::thread::sleep(Duration::from_millis(lightctl::RESUME_DELAY_MS + 1_000));
    let mut failed: Option<(LightingSettings, Instant)> = None;
    loop {
        std::thread::sleep(Duration::from_millis(IDLE_MS));
        let s = settings();
        let (p, prev, quitting) = {
            let rt = lights::lock(&shared);
            (rt.power, rt.shown, rt.quitting)
        };
        if quitting || !live::wanted(&s, &p) {
            continue;
        }
        if failed.as_ref().is_some_and(|(f, at)| *f == s && at.elapsed() < RETRY_AFTER) {
            continue;
        }
        if !LIVE.try_start() {
            continue;
        }
        let pauser = lightctl::alienfx::device::AwccPauser;
        let started = live::plan(&s, &p);
        rx.try_iter().for_each(drop);
        keys.set_listening(started.is_some_and(|pl| pl.needs_keys()));
        let mut presses = || rx.try_iter().collect::<Vec<u8>>();
        let mut last_check = 0u64;
        let mut stop = |now: u64| {
            if LIVE.should_stop() {
                return true;
            }
            if now.saturating_sub(last_check) < CHECK_MS {
                return false;
            }
            last_check = now;
            let fresh = settings();
            let rt = lights::lock(&shared);
            fresh != s || rt.quitting || live::plan(&fresh, &rt.power) != started
        };
        // Day and night's level moves during a run (`daylight_tick` in lib.rs updates the power state): the run
        // sends it itself, between its frames, so dusk never stops the animation.
        let mut power = || lights::lock(&shared).power;
        let r = live::run_windows(&mut SystemIo, &s, &p, prev, s.pause_awcc.then_some(&pauser as &dyn Pauser), &mut Steady(Instant::now()), Some(&mut presses), Some(&mut power), &mut stop);
        keys.set_listening(false);
        {
            let mut rt = lights::lock(&shared);
            match r {
                Ok(_) => {
                    // At the level the run ended on: the sun's, as it took it over.
                    rt.shown = Shown::of(&s, &PowerState { daylight: rt.power.daylight, ..p });
                    failed = None;
                }
                Err(e) => {
                    rt.last = Some(LastApply { ok: false, at: lightctl::now_ms(), text: format!("The live animation stopped: {e}") });
                    failed = Some((s.clone(), Instant::now()));
                }
            }
        }
        LIVE.finished();
    }
}
