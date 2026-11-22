//! The controller (design §5): the only owner of the firmware connection. It reads on its own thread and
//! publishes the latest snapshot; pipe threads only read what it published, and queue mode changes for it to
//! carry out (one connection, one writer). The watchdog watches its beat.
//!
//! The safety rules run here, on every step: the emergency rule (every fan to full while the hottest sensor
//! stays at or above the trigger) and the write watchdog (5 failed writes in a row, or no temperature for
//! 30 s, puts the laptop back on Balanced with every fan on the firmware's own curve).

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use wiregent_hw::control::{apply_emergency, reset_fans, set_fan_boost, set_profile, FirmwareWrite};
use wiregent_hw::reader::Reader;
use wiregent_hw::Firmware;
use wiregent_oem_alienware::rules::{interval_ms, EmergencyRule, RateLimit, Watchdog, EMERGENCY_MAX_C};
use wiregent_oem_alienware::Call;

/// What a SetFanBoost gets while the emergency rule holds every fan at full.
pub const HELD_BY_EMERGENCY: &str = "the emergency rule has every fan at full until temperatures come down, so fan boost changes are held until it releases";

/// The controller's firmware: passes everything through and notes whether each write went through, for the
/// watchdog. Every write the controller makes (requests, the emergency rule, restores) passes here.
pub struct Tap<F> {
    fw: F,
    results: RefCell<Vec<bool>>,
}

impl<F> Tap<F> {
    /// The write outcomes since the last call.
    fn take(&self) -> Vec<bool> {
        std::mem::take(&mut *self.results.borrow_mut())
    }
}

impl<F: Firmware> Firmware for Tap<F> {
    fn read(&self, c: Call) -> anyhow::Result<u32> {
        self.fw.read(c)
    }
}

impl<F: FirmwareWrite> FirmwareWrite for Tap<F> {
    fn write(&self, c: Call) -> anyhow::Result<u32> {
        let r = self.fw.write(c);
        self.results.borrow_mut().push(r.is_ok());
        r
    }
}

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
    reader: Reader<Tap<F>>,
    pub shared: Arc<Shared>,
    /// The machine gate for writes (model and BIOS verified), decided once at start.
    machine_ok: bool,
    limit: RateLimit,
    sleep: fn(std::time::Duration),
    emergency: EmergencyRule,
    /// The boosts the person set through SetFanBoost: what the emergency rule hands back on release.
    person: BTreeMap<u8, u8>,
    guard: Watchdog,
    /// When the watchdog's no-temperature clock started (the first step, or the last resume).
    guard_since: Option<u64>,
    /// The no-temperature restore already ran for this episode (it runs once, not every second).
    stale_restored: bool,
    /// How many watchdog restores ran (published with the snapshot).
    restores: u64,
}

impl<F: FirmwareWrite> Controller<F> {
    pub fn new(fw: F, shared: Arc<Shared>) -> anyhow::Result<Self> {
        Self::with_gate(fw, shared, false)
    }

    pub fn with_gate(fw: F, shared: Arc<Shared>, machine_ok: bool) -> anyhow::Result<Self> {
        let reader = Reader::new(Tap { fw, results: RefCell::new(Vec::new()) })?;
        *shared.lists.lock().unwrap() = Some(serde_json::to_value(reader.lists())?);
        Ok(Controller {
            reader,
            shared,
            machine_ok,
            limit: RateLimit::default(),
            sleep: std::thread::sleep,
            emergency: EmergencyRule::new(EMERGENCY_MAX_C),
            person: BTreeMap::new(),
            guard: Watchdog::default(),
            guard_since: None,
            stale_restored: false,
            restores: 0,
        })
    }

    /// Whether the emergency rule holds every fan at full right now.
    pub fn emergency_active(&self) -> bool {
        self.emergency.active()
    }

    /// Feeds the watchdog the outcome of every write since the last call; restores when it trips.
    fn note_writes(&mut self) {
        let mut trip = false;
        for ok in self.reader.firmware().take() {
            trip |= self.guard.write_result(ok);
        }
        if trip {
            self.restore();
        }
    }

    /// The watchdog's restore: Balanced and boost 0 on every fan (`Discovery::restore`). Exempt from the rate
    /// limit, gated on the verified machine like every write. Its own failures still count, so a restore that
    /// fails keeps the count at the limit and the next failed write asks again.
    fn restore(&mut self) {
        if !self.machine_ok {
            return;
        }
        let fw = self.reader.firmware();
        for c in self.reader.discovery.restore() {
            let _ = fw.write(c);
        }
        // An engaged emergency outranks the restore's boost 0: the mode goes to Balanced, but the fans stay at
        // full until the rule releases (with no temperature to read, nothing would release it sooner).
        if self.emergency.active() {
            let _ = apply_emergency(fw, &self.reader.discovery, wiregent_oem_alienware::rules::Emergency::Engage, &BTreeMap::new(), self.machine_ok);
        }
        for ok in fw.take() {
            self.guard.write_result(ok);
        }
        // The fans are on the firmware's curve now; an emergency release hands back that, not older boosts.
        self.person.clear();
        self.restores += 1;
    }

    /// The safety rules for one reading: `hottest` is the hottest valid sensor, None when none read.
    fn safety(&mut self, t_ms: u64, hottest: Option<i32>) {
        let since = *self.guard_since.get_or_insert(t_ms);
        if let Some(h) = hottest {
            self.guard.temperature_read(t_ms);
            self.stale_restored = false;
            if let Some(e) = self.emergency.push(t_ms, h) {
                // Errors are counted by the watchdog through the tap; nothing else to do with them here.
                let _ = apply_emergency(self.reader.firmware(), &self.reader.discovery, e, &self.person, self.machine_ok);
                self.note_writes();
            }
        }
        if !self.stale_restored && self.guard.stale(t_ms, since) {
            self.stale_restored = true;
            self.restore();
        }
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
            let (r, confirmed) = match cmd {
                Cmd::Profile(p) => match set_profile(fw, d, p, self.machine_ok, &mut self.limit, t_ms, &move |x| sleep(x)) {
                    Ok(c) => (Ok(serde_json::to_value(&c).unwrap_or_default()), c.confirmed),
                    Err(e) => (Err(e), false),
                },
                Cmd::FanBoost { .. } if !self.emergency.allows_fan_write() => (Err(anyhow::anyhow!(HELD_BY_EMERGENCY)), false),
                Cmd::FanBoost { fan, level } => match set_fan_boost(fw, d, fan, level, self.machine_ok, &mut self.limit, t_ms, &move |x| sleep(x)) {
                    Ok(c) => {
                        self.person.insert(fan, level);
                        (Ok(serde_json::to_value(&c).unwrap_or_default()), c.confirmed)
                    }
                    Err(e) => (Err(e), false),
                },
            };
            let _ = reply.send(r.map_err(|e| format!("{e:#}")));
            // One outcome per request that reached the firmware: the write went through and read back.
            // Requests refused before any write (gate, rate limit, emergency) don't count either way.
            let writes = self.reader.firmware().take();
            if !writes.is_empty() && self.guard.write_result(confirmed && writes.iter().all(|ok| *ok)) {
                self.restore();
            }
        }
    }

    /// One step at time `t_ms`; returns how long to wait before the next one.
    pub fn step(&mut self, t_ms: u64) -> u64 {
        self.shared.beat.store(t_ms, Ordering::SeqCst);
        if self.shared.resumed.swap(false, Ordering::SeqCst) {
            self.reader.resumed();
            // Fresh clocks: the sleep is neither hours without a temperature nor hours of heat. An engaged
            // emergency stays engaged until it sees 30 s of cool readings.
            self.guard_since = None;
            self.stale_restored = false;
            if !self.emergency.active() {
                self.emergency = EmergencyRule::new(self.emergency.trigger_c());
            }
        }
        if self.shared.is_paused() {
            return 1_000;
        }
        self.run_commands(t_ms);
        let snap = self.reader.tick(t_ms);
        // Only valid readings carry a value (an error or impossible answer reads as None).
        let hottest = snap.sensors.iter().filter_map(|s| s.celsius).max();
        self.safety(t_ms, hottest);
        if let Ok(mut v) = serde_json::to_value(&snap) {
            if let Some(o) = v.as_object_mut() {
                o.insert("emergency".into(), Value::Bool(self.emergency.active()));
                o.insert("restores".into(), Value::from(self.restores));
            }
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

/// `now_ms` is on the same clock as the beat, which starts at 0 when the service starts: a controller that
/// never beats (a firmware open that hangs) reads as a beat at 0 and is reported stuck `STUCK_MS` in.
pub fn watchdog(shared: &Shared, now_ms: u64) -> Health {
    let age = now_ms.saturating_sub(shared.beat());
    if age >= STUCK_MS {
        Health::Stuck { for_ms: age }
    } else {
        Health::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiregent_hw::replay::Replay;
    use wiregent_oem_alienware::{read_fan_boost, Method, OP_TEMPERATURE};

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
        c.step(1_000);
        c.shared.pause();
        fw.fail_op(OP_TEMPERATURE); // would make sensors go stale if read
        c.step(5_000);
        assert_eq!(c.shared.latest().unwrap().0, 1_000, "nothing new published while paused");
        assert_eq!(c.shared.beat(), 5_000, "the beat goes on while paused");
        assert_eq!(watchdog(&c.shared, 6_000), Health::Ok);
        assert_eq!(watchdog(&c.shared, 14_999), Health::Ok);
        assert_eq!(watchdog(&c.shared, 15_000), Health::Stuck { for_ms: 10_000 }, "a paused controller still has to beat");
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
        assert_eq!(watchdog(&c.shared, 9_999), Health::Ok, "still starting");
        assert_eq!(watchdog(&c.shared, 10_000), Health::Stuck { for_ms: 10_000 }, "never beat within 10 s of start: stuck");
        assert_eq!(watchdog(&c.shared, 50_000), Health::Stuck { for_ms: 50_000 });
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

    // ------------------------------------------------------------ the safety rules on the controller

    /// The replay firmware with two knobs it lacks (test-only, here rather than in crates/hw): every
    /// temperature reads `temp` when set, and the next `fail` writes fail.
    #[derive(Clone)]
    struct Bench {
        replay: Replay,
        temp: Arc<Mutex<Option<u32>>>,
        fail: Arc<AtomicU64>,
    }

    impl Bench {
        fn temp(&self, c: Option<u32>) {
            *self.temp.lock().unwrap() = c;
        }
        fn fail_next_writes(&self, n: u64) {
            self.fail.store(n, Ordering::SeqCst);
        }
        fn boost(&self, fan: u8) -> u32 {
            self.replay.read(read_fan_boost(fan)).unwrap()
        }
    }

    impl Firmware for Bench {
        fn read(&self, c: Call) -> anyhow::Result<u32> {
            if c.method == Method::ThermalInformation && c.op == OP_TEMPERATURE {
                if let Some(t) = *self.temp.lock().unwrap() {
                    return Ok(t);
                }
            }
            self.replay.read(c)
        }
    }

    impl FirmwareWrite for Bench {
        fn write(&self, c: Call) -> anyhow::Result<u32> {
            if self.fail.load(Ordering::SeqCst) > 0 {
                self.fail.fetch_sub(1, Ordering::SeqCst);
                anyhow::bail!("write failing (test)");
            }
            self.replay.write(c)
        }
    }

    /// A verified-machine controller on the October replay (CPU 100 °C unless `temp` says otherwise).
    fn bench() -> (Controller<Bench>, Bench) {
        let fw = Bench { replay: Replay::october(), temp: Default::default(), fail: Default::default() };
        (Controller::with_gate(fw.clone(), Arc::new(Shared::default()), true).unwrap().no_wait(), fw)
    }

    /// Steps once a second from `from` to `to`, both included.
    fn run(c: &mut Controller<Bench>, from: u64, to: u64) {
        for t in (from..=to).step_by(1_000) {
            c.step(t);
        }
    }

    fn boosts(c: &Controller<Bench>, fw: &Bench) -> Vec<u32> {
        c.reader.discovery.fans.iter().map(|f| fw.boost(*f)).collect()
    }

    fn restore_writes(c: &Controller<Bench>) -> Vec<u32> {
        c.reader.discovery.restore().iter().map(|x| x.packed()).collect()
    }

    /// C-07: 100 °C held for 5 s engages the rule and every fan goes to full.
    #[test]
    fn five_seconds_at_100_c_engages_and_every_fan_goes_to_full() {
        let (mut c, fw) = bench();
        run(&mut c, 0, 4_000);
        assert!(!c.emergency_active(), "4 s is not 5");
        assert!(fw.replay.writes().is_empty());
        c.step(5_000);
        assert!(c.emergency_active());
        assert_eq!(c.reader.discovery.fans.len(), 4);
        assert_eq!(boosts(&c, &fw), vec![100; 4]);
        assert_eq!(c.shared.latest().unwrap().1["emergency"], true);
    }

    /// While engaged a fan-boost request is refused and writes nothing; a mode switch still goes through.
    #[test]
    fn fan_boost_is_refused_while_engaged_but_modes_switch() {
        let (mut c, fw) = bench();
        run(&mut c, 0, 5_000);
        let before = fw.replay.writes().len();
        let boost = c.shared.request(Cmd::FanBoost { fan: 0x33, level: 40 });
        let mode = c.shared.request_profile(0xA1);
        c.step(6_000);
        assert_eq!(boost.recv().unwrap().unwrap_err(), HELD_BY_EMERGENCY);
        assert_eq!(mode.recv().unwrap().unwrap()["after"], "Performance");
        assert_eq!(fw.replay.writes().len(), before + 1, "only the mode switch was written");
        assert_eq!(fw.boost(0x33), 100);
    }

    /// C-09: 30 s at or below the release temperature (92 °C for the 98 °C trigger) releases, and each fan
    /// goes back to what the person set through SetFanBoost.
    #[test]
    fn thirty_seconds_cool_releases_and_restores_the_persons_boost() {
        let (mut c, fw) = bench();
        let mine = c.shared.request(Cmd::FanBoost { fan: 0x33, level: 60 });
        run(&mut c, 0, 5_000);
        assert_eq!(mine.recv().unwrap().unwrap()["after"], 60);
        assert_eq!(boosts(&c, &fw), vec![100; 4]);
        fw.temp(Some(92));
        run(&mut c, 6_000, 35_000);
        assert!(c.emergency_active(), "29 s cool is not 30");
        c.step(36_000);
        assert!(!c.emergency_active());
        assert_eq!(boosts(&c, &fw), vec![0, 60, 0, 0]);
        assert_eq!(c.shared.latest().unwrap().1["emergency"], false);
        // Released: the person's fan boost goes through again.
        let again = c.shared.request(Cmd::FanBoost { fan: 0x32, level: 30 });
        c.step(37_000);
        assert_eq!(again.recv().unwrap().unwrap()["after"], 30);
    }

    /// A spike above the release temperature during the cool hold starts the 30 s over.
    #[test]
    fn a_spike_restarts_the_release_hold() {
        let (mut c, fw) = bench();
        run(&mut c, 0, 5_000);
        fw.temp(Some(90));
        run(&mut c, 6_000, 20_000);
        fw.temp(Some(95));
        c.step(21_000);
        fw.temp(Some(90));
        run(&mut c, 22_000, 51_000);
        assert!(c.emergency_active(), "cool since 22 s: 29 s at 51 s");
        assert_eq!(boosts(&c, &fw), vec![100; 4]);
        c.step(52_000);
        assert!(!c.emergency_active());
        assert_eq!(boosts(&c, &fw), vec![0; 4]);
    }

    /// C-10: five failed writes in a row restore Balanced and boost 0 on every fan, even though the five
    /// used up this second's rate limit.
    #[test]
    fn five_failed_writes_restore_past_the_rate_limit() {
        let (mut c, fw) = bench();
        fw.temp(Some(60));
        fw.fail_next_writes(5);
        let asked: Vec<_> = (0..5).map(|i| c.shared.request(Cmd::FanBoost { fan: 0x32, level: 10 + i })).collect();
        c.step(0);
        assert!(asked.iter().all(|rx| rx.recv().unwrap().is_err()));
        assert!(!c.limit.try_write(0, false), "the second's budget is spent");
        assert_eq!(fw.replay.writes(), restore_writes(&c), "Balanced, then every fan to 0");
        let v = c.shared.latest().unwrap().1;
        assert_eq!((v["profile_name"].as_str(), v["restores"].as_u64()), (Some("Balanced"), Some(1)));
    }

    /// The count is of failures in a row: a write that goes through starts it over.
    #[test]
    fn a_successful_write_resets_the_failure_count() {
        let (mut c, fw) = bench();
        fw.temp(Some(60));
        fw.fail_next_writes(4);
        let asked: Vec<_> = (0..4).map(|i| c.shared.request(Cmd::FanBoost { fan: 0x32, level: 10 + i })).collect();
        c.step(0);
        assert!(asked.iter().all(|rx| rx.recv().unwrap().is_err()));
        let ok = c.shared.request(Cmd::FanBoost { fan: 0x32, level: 50 });
        c.step(1_000);
        assert_eq!(ok.recv().unwrap().unwrap()["confirmed"], true);
        fw.fail_next_writes(4);
        let asked: Vec<_> = (0..4).map(|i| c.shared.request(Cmd::FanBoost { fan: 0x32, level: 20 + i })).collect();
        c.step(2_000);
        assert!(asked.iter().all(|rx| rx.recv().unwrap().is_err()));
        assert_eq!(fw.replay.writes(), vec![0x0032_3202], "4 + 4 failures with a success between: no restore");
        // The fifth in a row does trip it.
        fw.fail_next_writes(1);
        let fifth = c.shared.request(Cmd::FanBoost { fan: 0x32, level: 70 });
        c.step(3_000);
        assert!(fifth.recv().unwrap().is_err());
        assert_eq!(fw.replay.writes()[1..], restore_writes(&c)[..]);
    }

    /// R-18: no valid temperature for 30 s restores, once per episode, both after readings stop and when
    /// they never started.
    #[test]
    fn thirty_seconds_without_a_temperature_restores_once() {
        let (mut c, fw) = bench();
        fw.temp(Some(60));
        c.step(0);
        fw.temp(None);
        fw.replay.fail_op(OP_TEMPERATURE);
        run(&mut c, 1_000, 29_000);
        assert!(fw.replay.writes().is_empty(), "29 s is not 30");
        c.step(30_000);
        assert_eq!(fw.replay.writes(), restore_writes(&c));
        run(&mut c, 31_000, 70_000);
        assert_eq!(fw.replay.writes().len(), restore_writes(&c).len(), "once per episode, not every second");

        let (mut c, fw) = bench();
        fw.replay.fail_op(OP_TEMPERATURE);
        run(&mut c, 1_000, 30_000);
        assert!(fw.replay.writes().is_empty(), "the clock starts at the first step");
        c.step(31_000);
        assert_eq!(fw.replay.writes(), restore_writes(&c));
    }

    /// Readings stop in the middle of an emergency: the restore sets Balanced, but the fans stay at full.
    #[test]
    fn a_restore_during_an_emergency_keeps_the_fans_at_full() {
        let (mut c, fw) = bench();
        run(&mut c, 0, 5_000);
        assert!(c.emergency_active());
        fw.replay.fail_op(OP_TEMPERATURE);
        run(&mut c, 6_000, 40_000);
        let v = c.shared.latest().unwrap().1;
        assert_eq!((v["profile_name"].as_str(), v["restores"].as_u64()), (Some("Balanced"), Some(1)));
        assert!(c.emergency_active());
        assert!(boosts(&c, &fw).iter().all(|b| *b == wiregent_hw::control::EMERGENCY_BOOST as u32), "{:?}", boosts(&c, &fw));
    }
}
