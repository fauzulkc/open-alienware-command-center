//! The controller (design §5): the only owner of the firmware connection. It reads on its own thread and
//! publishes the latest snapshot; pipe threads only read what it published, and queue mode changes for it to
//! carry out (one connection, one writer). The watchdog watches its beat.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use wiregent_hw::control::{reset_fans, set_fan_boost, set_profile, FirmwareWrite};
use wiregent_hw::reader::Reader;
use wiregent_oem_alienware::rules::{interval_ms, RateLimit};

type Reply = Sender<Result<serde_json::Value, String>>;

/// A change asked for over the pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Profile(u8),
    FanBoost { fan: u8, level: u8 },
}

/// What the controller publishes for the pipe threads.
#[derive(Default)]
pub struct Shared {
    latest: Mutex<Option<(u64, Value)>>,
    lists: Mutex<Option<Value>>,
    /// Last time (ms) the controller finished a step.
    beat: AtomicU64,
    paused: AtomicBool,
    resumed: AtomicBool,
    /// A client subscribed or asked recently: read every second.
    watched: AtomicBool,
    pub stop: AtomicBool,
    /// Mode changes asked for over the pipe, carried out by the controller in order.
    commands: Mutex<VecDeque<(Cmd, Reply)>>,
}

impl Shared {
    pub fn latest(&self) -> Option<(u64, Value)> {
        self.latest.lock().unwrap().clone()
    }
    pub fn lists(&self) -> Option<Value> {
        self.lists.lock().unwrap().clone()
    }
    pub fn beat(&self) -> u64 {
        self.beat.load(Ordering::SeqCst)
    }
    pub fn set_watched(&self, on: bool) {
        self.watched.store(on, Ordering::SeqCst);
    }
    /// Suspend (and Modern Standby on battery with the display off): stop reading the firmware.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }
    /// Resume: read again, with fresh staleness clocks and rule timers.
    pub fn resume(&self) {
        self.resumed.store(true, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    /// Queues a change for the controller; the answer arrives on the returned receiver.
    pub fn request(&self, cmd: Cmd) -> Receiver<Result<serde_json::Value, String>> {
        let (tx, rx) = channel();
        let mut q = self.commands.lock().unwrap();
        if q.len() >= 8 {
            let _ = tx.send(Err("too many changes waiting".into()));
        } else {
            q.push_back((cmd, tx));
        }
        rx
    }
    pub fn request_profile(&self, profile: u8) -> Receiver<Result<serde_json::Value, String>> {
        self.request(Cmd::Profile(profile))
    }
    fn has_commands(&self) -> bool {
        !self.commands.lock().unwrap().is_empty()
    }
}

pub struct Controller<F: FirmwareWrite> {
    reader: Reader<F>,
    pub shared: Arc<Shared>,
    /// The machine gate for writes (model and BIOS verified), decided once at start.
    machine_ok: bool,
    limit: RateLimit,
    sleep: fn(std::time::Duration),
}

impl<F: FirmwareWrite> Controller<F> {
    pub fn new(fw: F, shared: Arc<Shared>) -> anyhow::Result<Self> {
        Self::with_gate(fw, shared, false)
    }

    pub fn with_gate(fw: F, shared: Arc<Shared>, machine_ok: bool) -> anyhow::Result<Self> {
        let reader = Reader::new(fw)?;
        *shared.lists.lock().unwrap() = Some(serde_json::to_value(reader.lists())?);
        Ok(Controller { reader, shared, machine_ok, limit: RateLimit::default(), sleep: std::thread::sleep })
    }

    /// Tests: read-backs don't wait.
    pub fn no_wait(mut self) -> Self {
        self.sleep = |_| {};
        self
    }

    /// Carries out the queued mode changes (before the next reading, so it shows the result).
    fn run_commands(&mut self, t_ms: u64) {
        loop {
            let next = self.shared.commands.lock().unwrap().pop_front();
            let Some((cmd, reply)) = next else { break };
            let sleep = self.sleep;
            let (fw, d) = (self.reader.firmware(), &self.reader.discovery);
            let r = match cmd {
                Cmd::Profile(p) => set_profile(fw, d, p, self.machine_ok, &mut self.limit, t_ms, &move |x| sleep(x)).map(|c| serde_json::to_value(c).unwrap_or_default()),
                Cmd::FanBoost { fan, level } => set_fan_boost(fw, d, fan, level, self.machine_ok, &mut self.limit, t_ms, &move |x| sleep(x)).map(|c| serde_json::to_value(c).unwrap_or_default()),
            };
            let _ = reply.send(r.map_err(|e| format!("{e:#}")));
        }
    }

    /// One step at time `t_ms`; returns how long to wait before the next one.
    pub fn step(&mut self, t_ms: u64) -> u64 {
        self.shared.beat.store(t_ms, Ordering::SeqCst);
        if self.shared.resumed.swap(false, Ordering::SeqCst) {
            self.reader.resumed();
        }
        if self.shared.is_paused() {
            return 1_000;
        }
        self.run_commands(t_ms);
        let snap = self.reader.tick(t_ms);
        let hottest = snap.sensors.iter().filter_map(|s| s.celsius).max();
        if let Ok(v) = serde_json::to_value(&snap) {
            *self.shared.latest.lock().unwrap() = Some((t_ms, v));
        }
        self.shared.beat.store(t_ms, Ordering::SeqCst);
        interval_ms(hottest, self.shared.watched.load(Ordering::SeqCst))
    }

    /// Every fan back to the firmware's own curve (on stop, and when the service starts).
    pub fn reset_fans(&self) -> Vec<String> {
        if self.machine_ok {
            reset_fans(self.reader.firmware(), &self.reader.discovery)
        } else {
            Vec::new()
        }
    }

    /// Runs until `shared.stop`, starting and ending with every fan on the firmware's own curve.
    /// `now` is the service's steady clock in ms.
    pub fn run(mut self, now: impl Fn() -> u64) {
        self.reset_fans();
        while !self.shared.stop.load(Ordering::SeqCst) {
            let wait = self.step(now());
            let until = now() + wait;
            while now() < until && !self.shared.stop.load(Ordering::SeqCst) && !self.shared.has_commands() {
                std::thread::sleep(std::time::Duration::from_millis(100.min(wait)));
            }
        }
        self.reset_fans();
    }
}

/// What the watchdog concludes from the controller's beat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// No beat for this long: the controller is stuck (a hung firmware call past its timeout, or worse).
    /// In H1b this is logged and served to clients; from H2 the watchdog also restores on its own connection.
    Stuck { for_ms: u64 },
}

pub const STUCK_MS: u64 = 10_000;

pub fn watchdog(shared: &Shared, now_ms: u64) -> Health {
    let age = now_ms.saturating_sub(shared.beat());
    if shared.beat() != 0 && age >= STUCK_MS {
        Health::Stuck { for_ms: age }
    } else {
        Health::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiregent_hw::replay::Replay;
    use wiregent_oem_alienware::OP_TEMPERATURE;

    fn controller() -> (Controller<Replay>, Replay) {
        let fw = Replay::october();
        (Controller::new(fw.clone(), Arc::new(Shared::default())).unwrap(), fw)
    }

    #[test]
    fn a_step_publishes_a_snapshot_and_the_lists() {
        let (mut c, _) = controller();
        assert!(c.shared.latest().is_none());
        let wait = c.step(1_000);
        let (t, v) = c.shared.latest().unwrap();
        assert_eq!(t, 1_000);
        assert_eq!(v["profile_name"], "G-Mode");
        assert_eq!(wait, 1_000, "CPU at 100 °C: read every second");
        assert_eq!(c.shared.lists().unwrap()["profiles"][0], "0xA3");
    }

    /// Paused (sleep): no firmware reads, but the beat goes on so the watchdog stays quiet.
    #[test]
    fn paused_means_no_reads_and_resume_resets_staleness() {
        let (mut c, fw) = controller();
        c.step(0);
        c.shared.pause();
        fw.fail_op(OP_TEMPERATURE); // would make sensors go stale if read
        c.step(5_000);
        assert_eq!(c.shared.latest().unwrap().0, 0, "nothing new published while paused");
        assert_eq!(watchdog(&c.shared, 6_000), Health::Ok);
        // A long sleep, then resume: the first tick after waking must not call the sensors stale.
        c.shared.resume();
        c.step(3_600_000);
        let (_, v) = c.shared.latest().unwrap();
        assert!(v["sensors"].as_array().unwrap().iter().all(|s| s["stale"] == false), "{v}");
    }

    #[test]
    fn mode_changes_run_on_the_controller_and_only_past_the_gate() {
        let fw = Replay::october();
        let shared = Arc::new(Shared::default());
        let mut c = Controller::with_gate(fw.clone(), shared.clone(), true).unwrap().no_wait();
        let rx = shared.request_profile(0xA1);
        c.step(0);
        let r = rx.recv().unwrap().unwrap();
        assert_eq!(r["confirmed"], true);
        assert_eq!(r["after"], "Performance");
        assert_eq!(c.shared.latest().unwrap().1["profile_name"], "Performance", "the reading after the change shows it");
        let fw2 = Replay::october();
        let s2 = Arc::new(Shared::default());
        let mut c2 = Controller::with_gate(fw2.clone(), s2.clone(), false).unwrap().no_wait();
        let rx2 = s2.request_profile(0xA1);
        c2.step(0);
        assert!(rx2.recv().unwrap().is_err(), "unverified machine: refused");
        assert!(fw2.writes().is_empty());
    }

    #[test]
    fn fan_boost_runs_on_the_controller_and_reset_puts_it_back() {
        let fw = Replay::october();
        let shared = Arc::new(Shared::default());
        let mut c = Controller::with_gate(fw.clone(), shared.clone(), true).unwrap().no_wait();
        let rx = shared.request(Cmd::FanBoost { fan: 0x32, level: 80 });
        c.step(0);
        assert_eq!(rx.recv().unwrap().unwrap()["after"], 80);
        assert!(c.reset_fans().is_empty());
        assert_eq!(fw.writes().last(), Some(&0x0000_3202), "fan 0x32 back to 0");
    }

    #[test]
    fn the_watchdog_sees_a_stuck_controller() {
        let (mut c, _) = controller();
        assert_eq!(watchdog(&c.shared, 50_000), Health::Ok, "never started is not stuck");
        c.step(1_000);
        assert_eq!(watchdog(&c.shared, 10_999), Health::Ok);
        assert_eq!(watchdog(&c.shared, 11_000), Health::Stuck { for_ms: 10_000 });
    }

    #[test]
    fn watched_reads_every_second_even_when_cool() {
        let fw = Replay::from_capture(&wiregent_oem_alienware::REFERENCE_CAPTURE.replace("argr=0x00000064 (100)", "argr=0x00000032 (50)"));
        let mut c = Controller::new(fw, Arc::new(Shared::default())).unwrap();
        assert_eq!(c.step(0), 10_000, "CPU 50 °C, GPU 63 °C, nobody watching");
        c.shared.set_watched(true);
        assert_eq!(c.step(10_000), 1_000);
    }
}
