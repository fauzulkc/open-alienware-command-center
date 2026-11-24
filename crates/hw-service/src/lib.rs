//! The Wiregent hardware service (docs/plans/x17-hw-service-design.md). H1b: read-only.

pub mod audit;
pub mod controller;
pub mod identity;
pub use wiregent_hw::proto;
#[cfg(windows)]
pub mod pipe;

use controller::Shared;
use wiregent_hw::client_identity::Client;
use proto::{Request, Response};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// Writes one client (process id and program) may ask for in a rolling minute (design §5), on top of the
/// controller's 5 a second for everyone. The service's own safety writes never count.
pub const WRITES_PER_MINUTE: usize = 30;
const MINUTE_MS: u64 = 60_000;

/// Answers one request from what the controller published. Pipe threads never touch the firmware.
pub struct Answers {
    pub shared: std::sync::Arc<Shared>,
    pub health: Box<dyn Fn() -> serde_json::Value + Send + Sync>,
    health_cache: Mutex<Option<(Instant, serde_json::Value)>>,
    pub now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
    /// Where write lines go (C-03/C-04); none: not logged.
    audit: Option<audit::Sink>,
    /// When (on `now_ms`) each client's writes in the last minute were asked for.
    budget: Mutex<HashMap<(u32, PathBuf), VecDeque<u64>>>,
}

impl Answers {
    pub fn new(shared: std::sync::Arc<Shared>, health: Box<dyn Fn() -> serde_json::Value + Send + Sync>, now_ms: Box<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Answers { shared, health, health_cache: Mutex::new(None), now_ms, audit: None, budget: Mutex::new(HashMap::new()) }
    }

    /// Logs every write request: one line each, with the caller, the value and the outcome.
    pub fn with_audit(mut self, sink: audit::Sink) -> Self {
        self.audit = Some(sink);
        self
    }

    /// Takes one write from the client's budget, or says when it can ask again. Refusals don't count.
    fn spend(&self, client: &Client) -> Result<(), String> {
        let now = (self.now_ms)();
        let mut b = self.budget.lock().unwrap();
        b.retain(|_, q| {
            q.retain(|t| now.saturating_sub(*t) < MINUTE_MS);
            !q.is_empty()
        });
        let q = b.entry((client.pid, client.image.clone())).or_default();
        if q.len() >= WRITES_PER_MINUTE {
            let free_at = q.front().copied().unwrap_or(now) + MINUTE_MS;
            let secs = free_at.saturating_sub(now).div_ceil(1_000).max(1);
            return Err(format!("this program asked for {WRITES_PER_MINUTE} changes in the last minute, the most one program may; try again in {secs} s"));
        }
        q.push_back(now);
        Ok(())
    }

    /// A write request: the budget, then the controller; exactly one log line whatever happens.
    fn write(&self, client: &Client, op: &str, value: &str, run: impl FnOnce() -> Response) -> Response {
        let resp = match self.spend(client) {
            Ok(()) => run(),
            Err(message) => Response::Error { message },
        };
        if let Some(a) = &self.audit {
            let outcome = match &resp {
                Response::ModeChanged { change } | Response::BoostChanged { change } => format!("ok after={} confirmed={}", change["after"], change["confirmed"]),
                Response::Error { message } => format!("refused: {message}"),
                other => format!("unexpected answer {other:?}"),
            };
            a(audit::line(SystemTime::now(), audit::By::Client(client), op, value, &outcome));
        }
        resp
    }

    pub fn answer(&self, client: &Client, req: Request) -> Response {
        match req {
            Request::Hello { .. } => Response::Hello { proto: proto::PROTO, build: proto::BUILD.into() },
            Request::Snapshot => {
                self.shared.set_watched(true);
                let latest = self.shared.latest();
                let now = (self.now_ms)();
                Response::Snapshot { stale_ms: latest.as_ref().map(|(t, _)| now.saturating_sub(*t)), snapshot: latest.map(|(_, v)| v) }
            }
            Request::Lists => Response::Lists { lists: self.shared.lists().unwrap_or(serde_json::Value::Null) },
            Request::SetProfile { profile } => self.write(client, "set-profile", &profile, || {
                let Some(p) = wiregent_hw::control::parse_profile(&profile) else { return Response::Error { message: format!("'{profile}' isn't a mode id") } };
                match self.shared.request_profile(p).recv_timeout(std::time::Duration::from_secs(8)) {
                    Ok(Ok(change)) => Response::ModeChanged { change },
                    Ok(Err(message)) => Response::Error { message },
                    Err(_) => Response::Error { message: "the hardware controller didn't answer in time".into() },
                }
            }),
            Request::SetFanBoost { fan, level } => self.write(client, "set-fan-boost", &format!("fan={fan} level={level}"), || {
                let Some(f) = wiregent_hw::control::parse_profile(&fan) else { return Response::Error { message: format!("'{fan}' isn't a fan id") } };
                match self.shared.request(controller::Cmd::FanBoost { fan: f, level }).recv_timeout(std::time::Duration::from_secs(8)) {
                    Ok(Ok(change)) => Response::BoostChanged { change },
                    Ok(Err(message)) => Response::Error { message },
                    Err(_) => Response::Error { message: "the hardware controller didn't answer in time".into() },
                }
            }),
            Request::Health => {
                // Event-log and disk queries are slow: cached for a minute.
                let mut c = self.health_cache.lock().unwrap();
                if c.as_ref().is_none_or(|(at, _)| at.elapsed().as_secs() >= 60) {
                    *c = Some((Instant::now(), (self.health)()));
                }
                Response::Health { health: c.as_ref().map(|(_, v)| v.clone()).unwrap_or_default() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use controller::Controller;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use wiregent_hw::replay::Replay;

    /// Answers with a controller thread on a cool replay (no emergency) that carries out each queued change
    /// at once, a second apart on its own clock (so the 5-a-second limit never bites), a test clock for the
    /// budget, and a sink that keeps the log lines.
    struct Rig {
        answers: Answers,
        clock: Arc<AtomicU64>,
        lines: Arc<Mutex<Vec<String>>>,
        /// Set by the controller thread when it has nothing queued and is between steps.
        idle: Arc<AtomicBool>,
    }

    impl Rig {
        /// One request, sent only once the controller has finished the previous one. The controller runs every
        /// request queued at a step at that step's time, so a request sent while it was still finishing the
        /// last one used to land in the same simulated second and trip the 5-a-second limit (a flaky test).
        fn ask(&self, client: &Client, req: Request) -> Response {
            while !self.idle.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            self.idle.store(false, Ordering::SeqCst);
            let r = self.answers.answer(client, req);
            r
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.answers.shared.stop.store(true, Ordering::SeqCst);
        }
    }

    fn rig() -> Rig {
        let shared = Arc::new(controller::Shared::default());
        let s = shared.clone();
        let idle = Arc::new(AtomicBool::new(true));
        let idle_c = idle.clone();
        std::thread::spawn(move || {
            let fw = Replay::from_capture(&wiregent_oem_alienware::REFERENCE_CAPTURE.replace("argr=0x00000064 (100)", "argr=0x00000032 (50)"));
            let mut c = Controller::with_gate(fw, s.clone(), true).unwrap().no_wait();
            let mut t = 0;
            while !s.stop.load(Ordering::SeqCst) {
                if s.has_commands() {
                    t += 1_000;
                    c.step(t);
                } else {
                    idle_c.store(true, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        });
        let clock = Arc::new(AtomicU64::new(0));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let (c, l) = (clock.clone(), lines.clone());
        let answers = Answers::new(shared, Box::new(|| serde_json::Value::Null), Box::new(move || c.load(Ordering::SeqCst))).with_audit(Arc::new(move |m| l.lock().unwrap().push(m)));
        Rig { answers, clock, lines, idle }
    }

    fn app(pid: u32) -> Client {
        Client { pid, image: PathBuf::from(r"C:\Program Files\Wiregent\wiregent.exe"), sid: "S-1-5-21-1-2-3-1001".into() }
    }

    fn profile(p: &str) -> Request {
        Request::SetProfile { profile: p.into() }
    }

    /// C-03/C-04: every write request is exactly one line with the time, pid, program, request, value and
    /// outcome, whether it went through or was refused; reads log nothing.
    #[test]
    fn every_write_request_is_one_log_line_with_the_caller() {
        let r = rig();
        let me = app(4242);
        assert!(matches!(r.ask(&me, profile("0xA1")), Response::ModeChanged { .. }));
        assert!(matches!(r.ask(&me, Request::SetFanBoost { fan: "0x32".into(), level: 40 }), Response::BoostChanged { .. }));
        assert!(matches!(r.ask(&me, profile("0xZZ")), Response::Error { .. }));
        assert!(matches!(r.ask(&me, Request::Snapshot), Response::Snapshot { .. }));
        let lines = r.lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 3, "{lines:#?}");
        for l in &lines {
            assert!(l.starts_with("write 20") && l.contains("Z by=client pid=4242 image=wiregent.exe op="), "{l}");
        }
        assert!(lines[0].contains(r#"op=set-profile value=0xA1 outcome=ok after="Performance" confirmed=true"#), "{}", lines[0]);
        assert!(lines[1].contains("op=set-fan-boost value=fan=0x32 level=40 outcome=ok after=40 confirmed=true"), "{}", lines[1]);
        assert!(lines[2].contains("op=set-profile value=0xZZ outcome=refused: '0xZZ' isn't a mode id"), "{}", lines[2]);
    }

    /// Design §5: the 31st write in a minute is refused with when to retry, another client is unaffected,
    /// and the budget frees as the minute rolls on.
    #[test]
    fn thirty_writes_a_minute_per_client() {
        let r = rig();
        let (a, b) = (app(100), app(200));
        for i in 0..WRITES_PER_MINUTE as u64 {
            r.clock.store(i * 1_000, Ordering::SeqCst);
            let resp = r.ask(&a, profile(if i % 2 == 0 { "0xA1" } else { "0xA0" }));
            assert!(matches!(resp, Response::ModeChanged { .. }), "write {i}: {resp:?}");
        }
        r.clock.store(45_000, Ordering::SeqCst);
        let Response::Error { message } = r.ask(&a, profile("0xA1")) else { panic!("the 31st write went through") };
        assert!(message.contains("30 changes in the last minute") && message.ends_with("try again in 15 s"), "{message}");
        assert!(matches!(r.ask(&a, Request::SetFanBoost { fan: "0x32".into(), level: 10 }), Response::Error { .. }), "fan boosts share the budget");
        assert!(matches!(r.ask(&b, profile("0xA1")), Response::ModeChanged { .. }), "another client is unaffected");
        let same_program_other_pid = app(101);
        assert!(matches!(r.ask(&same_program_other_pid, profile("0xA0")), Response::ModeChanged { .. }));
        // The first write (at 0 s) leaves the window at 60 s: one more goes through, the next waits for 61 s.
        r.clock.store(60_000, Ordering::SeqCst);
        assert!(matches!(r.ask(&a, profile("0xA1")), Response::ModeChanged { .. }), "the budget frees after the minute");
        let Response::Error { message } = r.ask(&a, profile("0xA0")) else { panic!("only one write had freed") };
        assert!(message.ends_with("try again in 1 s"), "{message}");
        assert_eq!(r.lines.lock().unwrap().iter().filter(|l| l.contains("outcome=refused: this program asked for 30")).count(), 3, "refusals are logged too");
    }
}
