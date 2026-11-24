//! The reactive glow in the app (docs/lighting-control.md, "Reactive glow"): keys warm up as they are
//! pressed. The model, the delta frames and the runner are cockpit-core's `reactive`, shared with
//! `cockpit lighting reactive`; this is the thread that drives it.
//!
//! - Key-downs come from the boost key's Raw Input listener (read-only, `RIDEV_INPUTSINK`) through [`Keys`]:
//!   while the glow runs each becomes an LED id on a channel; otherwise it is dropped at once. Nothing logs
//!   or stores which keys were pressed.
//! - The runner lives in the lighting runtime (`lights::Runtime::glow`), so each frame is written under the
//!   one lighting lock and never interleaves with the app's other writes. A settings write carries the glow
//!   on from the new static colours or ends it; restoring AWCC's look ends it.
//! - Power: a dark trigger (sleep, lid, display off) writes the keyboard dark and ends the glow; once the
//!   look is back after wake, this thread starts it again. Quitting stops it with the static colours written
//!   back (`lights::quit`).
//! - A start or frame that fails is shown as the last apply and isn't tried again until the settings change
//!   or another write succeeds.

use crate::lights::{self, Shared};
use cockpit_core::keyinput;
use cockpit_core::lightctl::{self, alienfx::device::Pauser, LastApply, LightingSettings, SystemIo};
use cockpit_core::reactive::{self, Runner};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the runner's settings may differ from the saved ones (or the glow be unwanted) before it ends
/// without a write: a save is followed by its own write within this, which carries the glow on.
const GRACE_MS: u64 = 1_000;
/// The wait between checks while the glow isn't running, ms.
const IDLE_WAIT_MS: u64 = 250;

/// Key-downs from the Raw Input listener to the glow thread.
pub struct Keys {
    listening: AtomicBool,
    tx: Mutex<Sender<u8>>,
}

impl Keys {
    pub fn new() -> (Keys, Receiver<u8>) {
        let (tx, rx) = channel();
        (Keys { listening: AtomicBool::new(false), tx: Mutex::new(tx) }, rx)
    }

    /// A key-down (Raw Input make code and flags): its LED id to the glow while it runs; dropped otherwise.
    pub fn key_down(&self, make: u16, flags: u16) {
        if !self.listening.load(Ordering::Relaxed) {
            return;
        }
        if let Some(id) = keyinput::led_for(make, flags) {
            if let Ok(tx) = self.tx.lock() {
                let _ = tx.send(id);
            }
        }
    }
}

fn failed(text: String) -> LastApply {
    LastApply { ok: false, at: lightctl::now_ms(), text }
}

/// One round under the lighting lock: start the glow, send a frame, or end it. Returns how long to wait, ms.
fn round(shared: &Shared, s: &LightingSettings, keys: &Keys, rx: &Receiver<u8>, differ_since: &mut Option<u64>) -> u64 {
    let mut rt = lights::lock(shared);
    let now = rt.now();
    let p = rt.power;
    keys.listening.store(rt.glow.is_some() && !rt.quitting, Ordering::Relaxed);
    if rt.quitting {
        return IDLE_WAIT_MS;
    }
    let Some(r) = rt.glow.as_mut() else {
        *differ_since = None;
        rx.try_iter().for_each(drop);
        if reactive::wanted(s, &p) && rt.glow_failed.as_ref() != Some(s) {
            let pauser = lightctl::alienfx::device::AwccPauser;
            let prev = rt.shown;
            match Runner::start(&mut SystemIo, s, &p, prev, s.pause_awcc.then_some(&pauser as &dyn Pauser), now) {
                Ok(r) => {
                    let wait = r.due_ms().saturating_sub(now);
                    rt.glow = Some(r);
                    keys.listening.store(true, Ordering::Relaxed);
                    return wait;
                }
                Err(e) => {
                    rt.glow_failed = Some(s.clone());
                    rt.last = Some(failed(format!("The reactive glow couldn't start: {e}")));
                }
            }
        }
        return IDLE_WAIT_MS;
    };
    // Saved settings the runner doesn't have yet, or a state it can't run in: the write that follows a save
    // carries it on or ends it. Without one (a save that writes nothing) it ends here, with no write; with the
    // glow still wanted, the next round starts it again from the saved settings.
    if r.settings() != s || !reactive::wanted(s, &p) {
        let since = *differ_since.get_or_insert(now);
        if now.saturating_sub(since) < GRACE_MS {
            return 20;
        }
        *differ_since = None;
        rt.end_glow();
        return 1;
    }
    *differ_since = None;
    for id in rx.try_iter() {
        r.press(id, now);
    }
    match r.step(now) {
        Ok(_) => r.due_ms().saturating_sub(now),
        Err(e) => {
            let r = rt.glow.take().expect("running");
            keys.listening.store(false, Ordering::Relaxed);
            let text = r.fail(&mut SystemIo, e);
            rt.glow_failed = Some(s.clone());
            rt.last = Some(failed(text));
            IDLE_WAIT_MS
        }
    }
}

/// The glow's thread: forever, a round and then a wait (a frame interval while it runs).
pub fn run(shared: Shared, keys: Arc<Keys>, rx: Receiver<u8>, settings: impl Fn() -> LightingSettings) {
    let mut differ_since = None;
    loop {
        let s = settings();
        let wait = round(&shared, &s, &keys, &rx, &mut differ_since);
        std::thread::sleep(Duration::from_millis(wait.clamp(1, IDLE_WAIT_MS)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// While the glow isn't running a key-down is dropped without being mapped; while it runs it becomes an
    /// LED id and nothing else.
    #[test]
    fn keys_reach_the_glow_only_while_it_runs() {
        let (keys, rx) = Keys::new();
        keys.key_down(0x10, 0); // Q
        assert!(rx.try_recv().is_err(), "not running: dropped");
        keys.listening.store(true, Ordering::Relaxed);
        keys.key_down(0x10, 0);
        keys.key_down(0x1D, keyinput::RI_KEY_E0); // Right Ctrl
        keys.key_down(0x39, 0); // space: no LED
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![42, 112]);
    }
}
