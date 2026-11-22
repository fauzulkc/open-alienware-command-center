//! The service's safety rules, as pure state machines fed with readings (docs/plans/plugins-v1.md §6, the
//! "gate fixes" numbers). None of these can be turned off or widened by a plugin call or the advisor: they
//! have no setting except the emergency trigger, which can only be lowered.

use crate::Discovery;
use serde::Serialize;
use std::collections::BTreeMap;

/// One reading of every sensor and fan at a moment (milliseconds on any steady clock).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sample {
    pub t_ms: u64,
    /// sensor id -> degrees C
    pub temps: BTreeMap<u8, i32>,
    /// fan id -> RPM
    pub rpm: BTreeMap<u8, u32>,
}

impl Sample {
    pub fn hottest(&self) -> Option<i32> {
        self.temps.values().copied().max()
    }
}

// ---------------------------------------------------------------- fan-pair fault (H1, cases R-07..R-09)

/// A fan that stood still while its sensor was hot and its partner on the same sensor was spinning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FanFault {
    pub fan: u8,
    pub sensor: u8,
    pub from_ms: u64,
    pub to_ms: u64,
    pub peak_c: i32,
}

/// Watches for "likely fan fault": 0 RPM for at least `window_ms` while the paired sensor is above `hot_c`
/// and a partner fan runs at `partner_min_rpm` or more. Fires once per episode, when it crosses the window.
#[derive(Debug, Clone)]
pub struct FanPairWatch {
    pub window_ms: u64,
    pub hot_c: i32,
    pub partner_min_rpm: u32,
    open: BTreeMap<u8, (u64, i32, bool)>, // fan -> (since, peak, already reported)
}

impl Default for FanPairWatch {
    fn default() -> Self {
        FanPairWatch { window_ms: 120_000, hot_c: 80, partner_min_rpm: 1000, open: BTreeMap::new() }
    }
}

impl FanPairWatch {
    /// Feeds one sample; returns the faults that just crossed the window.
    pub fn push(&mut self, d: &Discovery, s: &Sample) -> Vec<FanFault> {
        let mut fired = Vec::new();
        for &fan in &d.fans {
            let Some(&sensor) = d.fan_sensor.get(&fan) else { continue };
            let temp = s.temps.get(&sensor).copied();
            let stopped = s.rpm.get(&fan) == Some(&0);
            let partner_spins = d.partners(fan).iter().any(|p| s.rpm.get(p).is_some_and(|r| *r >= self.partner_min_rpm));
            let hot = temp.is_some_and(|t| t > self.hot_c);
            if stopped && hot && partner_spins {
                let entry = self.open.entry(fan).or_insert((s.t_ms, i32::MIN, false));
                entry.1 = entry.1.max(temp.unwrap_or(i32::MIN));
                if !entry.2 && s.t_ms.saturating_sub(entry.0) >= self.window_ms {
                    entry.2 = true;
                    fired.push(FanFault { fan, sensor, from_ms: entry.0, to_ms: s.t_ms, peak_c: entry.1 });
                }
            } else {
                self.open.remove(&fan);
            }
        }
        fired
    }

    /// Every fault in a whole trace (the health card's history view).
    pub fn scan(d: &Discovery, samples: &[Sample]) -> Vec<FanFault> {
        let mut w = FanPairWatch::default();
        samples.iter().flat_map(|s| w.push(d, s)).collect()
    }
}

// ---------------------------------------------------------------- every fan on a hot sensor stopped

/// Every fan that cools a sensor read 0 RPM while that sensor was hot. The fan-pair rule can't see this (it
/// needs a spinning partner), and it's the worse case: nothing is moving air over that heatsink. Seen on the
/// owner's x17 R2 from 2026-11-13: both CPU fans at 0 RPM with the CPU at 100 °C in every mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZoneStopped {
    pub sensor: u8,
    pub fans: Vec<u8>,
    pub from_ms: u64,
    pub to_ms: u64,
    pub peak_c: i32,
}

/// Fires once per episode when every fan on a sensor has read 0 RPM for `window_ms` while the sensor read
/// above `hot_c`. A fan without a reading this sample breaks the episode (it might be spinning).
#[derive(Debug, Clone)]
pub struct ZoneStoppedWatch {
    pub window_ms: u64,
    pub hot_c: i32,
    open: BTreeMap<u8, (u64, i32, bool)>, // sensor -> (since, peak, already reported)
}

impl Default for ZoneStoppedWatch {
    fn default() -> Self {
        ZoneStoppedWatch { window_ms: 30_000, hot_c: 80, open: BTreeMap::new() }
    }
}

impl ZoneStoppedWatch {
    pub fn push(&mut self, d: &Discovery, s: &Sample) -> Vec<ZoneStopped> {
        let mut zones: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        for &fan in &d.fans {
            if let Some(&sensor) = d.fan_sensor.get(&fan) {
                zones.entry(sensor).or_default().push(fan);
            }
        }
        let mut fired = Vec::new();
        for (sensor, fans) in zones {
            let temp = s.temps.get(&sensor).copied();
            let all_stopped = fans.iter().all(|f| s.rpm.get(f) == Some(&0));
            match temp {
                Some(t) if t > self.hot_c && all_stopped => {
                    let e = self.open.entry(sensor).or_insert((s.t_ms, t, false));
                    e.1 = e.1.max(t);
                    if !e.2 && s.t_ms.saturating_sub(e.0) >= self.window_ms {
                        e.2 = true;
                        fired.push(ZoneStopped { sensor, fans: fans.clone(), from_ms: e.0, to_ms: s.t_ms, peak_c: e.1 });
                    }
                }
                _ => {
                    self.open.remove(&sensor);
                }
            }
        }
        fired
    }

    pub fn scan(d: &Discovery, samples: &[Sample]) -> Vec<ZoneStopped> {
        let mut w = ZoneStoppedWatch::default();
        samples.iter().flat_map(|s| w.push(d, s)).collect()
    }
}

// ---------------------------------------------------------------- stuck speed reading (review 2026-11-09)

/// A fan whose reported speed doesn't move while its partner on the same sensor does: the value is a setpoint
/// or a dead tach, not a measurement. Fan 0x3C read exactly 3000 for 18 minutes in G-Mode (exactly 2000 in
/// Balanced) while its partner 0x33 moved between 700 and 1500. Comparing with the partner, not the sensor,
/// keeps a fan legitimately pinned at full speed (0x32 at 4600 in G-Mode) from being flagged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StuckReading {
    pub fan: u8,
    pub rpm: u32,
    pub from_ms: u64,
    pub to_ms: u64,
    /// How far the partner fan's speed moved meanwhile (RPM).
    pub partner_swing_rpm: u32,
}

/// Fires once per episode when a fan reports the same non-zero RPM for `window_ms` while a partner fan's
/// speed moved at least `partner_swing_rpm`. A stopped fan (0 RPM) is the fan-pair rule's business.
#[derive(Debug, Clone)]
pub struct StuckWatch {
    pub window_ms: u64,
    pub partner_swing_rpm: u32,
    open: BTreeMap<u8, (u32, u64, u32, u32, bool)>, // fan -> (rpm, since, partner min, partner max, reported)
}

impl Default for StuckWatch {
    fn default() -> Self {
        StuckWatch { window_ms: 60_000, partner_swing_rpm: 300, open: BTreeMap::new() }
    }
}

impl StuckWatch {
    pub fn push(&mut self, d: &Discovery, s: &Sample) -> Vec<StuckReading> {
        let mut fired = Vec::new();
        for &fan in &d.fans {
            let partner = d.partners(fan).iter().filter_map(|p| s.rpm.get(p).copied()).max();
            let (Some(&rpm), Some(partner)) = (s.rpm.get(&fan), partner) else {
                self.open.remove(&fan);
                continue;
            };
            if rpm == 0 {
                self.open.remove(&fan);
                continue;
            }
            let e = self.open.entry(fan).or_insert((rpm, s.t_ms, partner, partner, false));
            if e.0 != rpm {
                *e = (rpm, s.t_ms, partner, partner, false);
                continue;
            }
            e.2 = e.2.min(partner);
            e.3 = e.3.max(partner);
            if !e.4 && s.t_ms.saturating_sub(e.1) >= self.window_ms && e.3 - e.2 >= self.partner_swing_rpm {
                e.4 = true;
                fired.push(StuckReading { fan, rpm, from_ms: e.1, to_ms: s.t_ms, partner_swing_rpm: e.3 - e.2 });
            }
        }
        fired
    }

    pub fn scan(d: &Discovery, samples: &[Sample]) -> Vec<StuckReading> {
        let mut w = StuckWatch::default();
        samples.iter().flat_map(|s| w.push(d, s)).collect()
    }
}

// ---------------------------------------------------------------- emergency rule (safety 4, C-07..C-09)

pub const EMERGENCY_MAX_C: i32 = 98;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Emergency {
    /// Boost every fan to 255, whatever the person picked.
    Engage,
    /// Hand the fans back to the person's choice.
    Release,
}

/// Any sensor at or above the trigger for 5 s → engage; at or below 92 C held for 30 s → release.
#[derive(Debug, Clone)]
pub struct EmergencyRule {
    trigger_c: i32,
    pub hold_ms: u64,
    pub release_c: i32,
    pub release_ms: u64,
    active: bool,
    hot_since: Option<u64>,
    cool_since: Option<u64>,
}

impl EmergencyRule {
    /// The trigger can be lowered from the plugin's `emergency_c` setting, never raised past 98 C.
    /// Release is tied to the trigger (6 °C under it), so a lowered trigger can't make the fans swing between
    /// full and normal every half minute.
    pub fn new(trigger_c: i32) -> Self {
        let trigger_c = trigger_c.min(EMERGENCY_MAX_C);
        EmergencyRule { trigger_c, hold_ms: 5_000, release_c: trigger_c - 6, release_ms: 30_000, active: false, hot_since: None, cool_since: None }
    }

    pub fn trigger_c(&self) -> i32 {
        self.trigger_c
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// A person's (or anyone's) fan write is held back while the rule is engaged.
    pub fn allows_fan_write(&self) -> bool {
        !self.active
    }

    pub fn push(&mut self, t_ms: u64, hottest: i32) -> Option<Emergency> {
        if !self.active {
            if hottest >= self.trigger_c {
                let since = *self.hot_since.get_or_insert(t_ms);
                if t_ms.saturating_sub(since) >= self.hold_ms {
                    self.active = true;
                    self.hot_since = None;
                    self.cool_since = None;
                    return Some(Emergency::Engage);
                }
            } else {
                self.hot_since = None;
            }
        } else if hottest <= self.release_c {
            let since = *self.cool_since.get_or_insert(t_ms);
            if t_ms.saturating_sub(since) >= self.release_ms {
                self.active = false;
                self.cool_since = None;
                return Some(Emergency::Release);
            }
        } else {
            self.cool_since = None;
        }
        None
    }
}

// ---------------------------------------------------------------- watchdog (safety 3, C-10, R-18)

/// Trips after 5 *consecutive* failed writes, or when no temperature has been read for 30 s.
#[derive(Debug, Clone)]
pub struct Watchdog {
    pub limit: u32,
    pub stale_ms: u64,
    failures: u32,
    last_temp_ms: Option<u64>,
}

impl Default for Watchdog {
    fn default() -> Self {
        Watchdog { limit: 5, stale_ms: 30_000, failures: 0, last_temp_ms: None }
    }
}

impl Watchdog {
    /// Returns true when this failure trips the watchdog (restore now).
    pub fn write_result(&mut self, ok: bool) -> bool {
        if ok {
            self.failures = 0;
            return false;
        }
        self.failures += 1;
        // At the limit and on every failure after it, so a restore that also fails keeps asking to restore.
        self.failures >= self.limit
    }

    pub fn temperature_read(&mut self, t_ms: u64) {
        self.last_temp_ms = Some(t_ms);
    }

    /// Readings are stale (the service started counting at `started_ms`).
    pub fn stale(&self, now_ms: u64, started_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_temp_ms.unwrap_or(started_ms)) >= self.stale_ms
    }
}

// ---------------------------------------------------------------- rate limit (C-11) and sampling (R-20)

/// At most 5 writes in any rolling second. Watchdog and emergency writes are exempt and never refused.
#[derive(Debug, Clone, Default)]
pub struct RateLimit {
    recent: Vec<u64>,
}

impl RateLimit {
    pub const PER_SECOND: usize = 5;

    pub fn try_write(&mut self, now_ms: u64, exempt: bool) -> bool {
        if exempt {
            return true;
        }
        self.recent.retain(|t| now_ms.saturating_sub(*t) < 1_000);
        if self.recent.len() >= Self::PER_SECOND {
            return false;
        }
        self.recent.push(now_ms);
        true
    }
}

/// How often to read: every second when warm (≥ 80 C: a 12700H can climb 20 °C in a few seconds), watched,
/// loaded or auto-tuning; every 10 s otherwise.
pub fn interval_ms(hottest: Option<i32>, watched_or_busy: bool) -> u64 {
    if watched_or_busy || hottest.is_none_or(|t| t >= 80) {
        1_000
    } else {
        10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F2: &str = include_str!("../tests/fixtures/f2-fan-fault-2026-10-07.csv");

    fn x17() -> Discovery {
        crate::tests::x17()
    }

    /// F2 rows: timestamp, sensor 0x01, sensor 0x06, fans 0x32, 0x33, 0x3B, 0x3C, profile.
    fn f2() -> Vec<Sample> {
        F2.lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let c: Vec<&str> = l.split(',').collect();
                let time = c[0].split('T').nth(1).unwrap();
                let hms: Vec<f64> = time.split(':').map(|x| x.parse().unwrap()).collect();
                let t_ms = ((hms[0] * 3600.0 + hms[1] * 60.0 + hms[2]) * 1000.0) as u64;
                let n = |i: usize| c[i].parse::<i64>().unwrap();
                Sample {
                    t_ms,
                    temps: BTreeMap::from([(0x01, n(1) as i32), (0x06, n(2) as i32)]),
                    rpm: BTreeMap::from([(0x32, n(3) as u32), (0x33, n(4) as u32), (0x3B, n(5) as u32), (0x3C, n(6) as u32)]),
                }
            })
            .collect()
    }

    /// F3s: F2 with fan 0x3B given its partner 0x32's speed (synthetic healthy trace; replace with F3r).
    fn f3s() -> Vec<Sample> {
        f2().into_iter().map(|mut s| {
            let r = s.rpm[&0x32];
            s.rpm.insert(0x3B, r);
            s
        }).collect()
    }

    #[test]
    fn the_fault_trace_is_what_the_fixture_readme_says() {
        let s = f2();
        assert_eq!(s.len(), 535);
        assert!(s.iter().all(|x| x.rpm[&0x3B] == 0));
        assert_eq!(s.iter().filter(|x| x.temps[&0x01] >= 98).count(), 525);
    }

    /// R-07: the dead fan is reported once, about two minutes in, on the CPU sensor.
    #[test]
    fn the_fan_pair_rule_fires_once_on_the_fault_trace() {
        let faults = FanPairWatch::scan(&x17(), &f2());
        assert_eq!(faults.len(), 1, "{faults:?}");
        let f = &faults[0];
        assert_eq!((f.fan, f.sensor), (0x3B, 0x01));
        assert!((120_000..=125_000).contains(&(f.to_ms - f.from_ms)), "{}", f.to_ms - f.from_ms);
        assert!(f.peak_c >= 99);
    }

    /// R-08, R-09: silent on a healthy trace, on a cool machine, and when both fans of a pair are stopped.
    #[test]
    fn the_fan_pair_rule_is_silent_when_it_should_be() {
        assert!(FanPairWatch::scan(&x17(), &f3s()).is_empty());
        let cool: Vec<Sample> = f2().into_iter().map(|mut s| { s.temps.insert(0x01, 60); s }).collect();
        assert!(FanPairWatch::scan(&x17(), &cool).is_empty());
        let both_stopped: Vec<Sample> = f2().into_iter().map(|mut s| { s.rpm.insert(0x32, 0); s }).collect();
        assert!(FanPairWatch::scan(&x17(), &both_stopped).is_empty(), "both fans stopped is the zone rule's case, not a one-fan fault");
    }

    /// Both CPU fans at 0 RPM on the hot trace: one alert, on the CPU sensor, naming both fans, 30 s in.
    #[test]
    fn every_fan_on_a_hot_sensor_stopped_fires_once() {
        let both_stopped: Vec<Sample> = f2().into_iter().map(|mut s| { s.rpm.insert(0x32, 0); s }).collect();
        let z = ZoneStoppedWatch::scan(&x17(), &both_stopped);
        assert_eq!(z.len(), 1, "{z:?}");
        assert_eq!((z[0].sensor, z[0].fans.clone()), (0x01, vec![0x32, 0x3B]));
        assert!((30_000..=33_000).contains(&(z[0].to_ms - z[0].from_ms)), "{}", z[0].to_ms - z[0].from_ms);
        assert!(z[0].peak_c >= 99);
    }

    /// Silent when a fan on the sensor spins (F2 itself: 0x32 at 4600), when it's cool, and for the GPU fans
    /// idling at 0 RPM on a cool GPU. 80 °C is still cool; 81 is hot.
    #[test]
    fn the_zone_rule_is_silent_when_it_should_be() {
        let d = x17();
        assert!(ZoneStoppedWatch::scan(&d, &f2()).is_empty(), "one CPU fan spins");
        let at = |c: i32| -> Vec<Sample> { f2().into_iter().map(|mut s| { s.rpm.insert(0x32, 0); s.temps.insert(0x01, c); s }).collect() };
        assert!(ZoneStoppedWatch::scan(&d, &at(80)).is_empty());
        assert_eq!(ZoneStoppedWatch::scan(&d, &at(81)).len(), 1);
        let gpu_idle: Vec<Sample> = f3s().into_iter().map(|mut s| { s.rpm.insert(0x33, 0); s.rpm.insert(0x3C, 0); s.temps.insert(0x06, 55); s }).collect();
        assert!(ZoneStoppedWatch::scan(&d, &gpu_idle).is_empty(), "GPU fans off on a cool GPU is normal");
    }

    /// A missing reading breaks the episode (that fan may be spinning); a dip in temperature does too.
    #[test]
    fn the_zone_rule_restarts_after_a_break() {
        let d = x17();
        let mut w = ZoneStoppedWatch::default();
        let s = |t: u64, c: i32, rpm: Option<u32>| {
            let mut x = Sample { t_ms: t, temps: BTreeMap::from([(0x01, c), (0x06, 60)]), rpm: BTreeMap::from([(0x32, 0), (0x33, 1000), (0x3C, 2000)]) };
            if let Some(r) = rpm {
                x.rpm.insert(0x3B, r);
            }
            x
        };
        assert!(w.push(&d, &s(0, 95, Some(0))).is_empty());
        assert!(w.push(&d, &s(20_000, 95, None)).is_empty(), "0x3B unread: not proven stopped");
        assert!(w.push(&d, &s(29_000, 95, Some(0))).is_empty());
        assert!(w.push(&d, &s(50_000, 95, Some(0))).is_empty(), "only 21 s since the break");
        assert_eq!(w.push(&d, &s(59_000, 95, Some(0))).len(), 1);
        assert!(w.push(&d, &s(90_000, 95, Some(0))).is_empty(), "once per episode");
        assert!(w.push(&d, &s(91_000, 70, Some(0))).is_empty());
        assert!(w.push(&d, &s(92_000, 95, Some(0))).is_empty());
        assert_eq!(w.push(&d, &s(122_000, 95, Some(0))).len(), 1, "a new episode after cooling down");
    }

    /// Fan 0x3C's exact 3000 RPM in the fault trace is flagged as a reading that doesn't move.
    #[test]
    fn a_speed_reading_that_never_moves_is_flagged() {
        let d = x17();
        let found = StuckWatch::scan(&d, &f2());
        assert_eq!(found.iter().map(|s| (s.fan, s.rpm)).collect::<Vec<_>>(), vec![(0x3C, 3000)], "{found:?}");
        assert!(found[0].partner_swing_rpm >= 300);
        // 0x32 pinned at 4600 isn't flagged (its partner 0x3B never moved), and with 0x33 steady too, 0x3C's
        // constant proves nothing.
        let steady: Vec<Sample> = f2().into_iter().map(|mut s| { s.rpm.insert(0x33, 1000); s }).collect();
        assert!(StuckWatch::scan(&d, &steady).is_empty());
    }

    /// C-07: five seconds at the trigger engages; a dip below it starts the count again.
    #[test]
    fn the_emergency_rule_engages_after_five_hot_seconds() {
        let mut r = EmergencyRule::new(98);
        let got: Vec<_> = (0..=5).map(|t| r.push(t * 1000, 98)).collect();
        assert_eq!(got, vec![None, None, None, None, None, Some(Emergency::Engage)]);
        assert!(!r.allows_fan_write());

        let mut r = EmergencyRule::new(98);
        for t in 0..4 {
            assert_eq!(r.push(t * 1000, 99), None);
        }
        assert_eq!(r.push(4_000, 97), None);
        assert_eq!(r.push(5_000, 99), None, "the dip reset the five seconds");
        assert!(!r.active());
    }

    /// C-09: release only at or below 92 C held 30 s; a spike in between restarts the hold.
    #[test]
    fn the_emergency_rule_releases_after_thirty_cool_seconds() {
        let mut r = EmergencyRule::new(98);
        for t in 0..=5 {
            r.push(t * 1000, 99);
        }
        assert!(r.active());
        assert_eq!(r.push(10_000, 92), None);
        assert_eq!(r.push(30_000, 95), None, "93-97 holds the boost");
        assert_eq!(r.push(31_000, 90), None);
        assert_eq!(r.push(60_000, 90), None);
        assert_eq!(r.push(61_000, 91), Some(Emergency::Release));
        assert!(r.allows_fan_write());
    }

    /// The trigger can be lowered by the setting, never raised past 98 C.
    #[test]
    fn the_emergency_trigger_can_only_be_lowered() {
        assert_eq!(EmergencyRule::new(120).trigger_c(), 98);
        assert_eq!(EmergencyRule::new(90).trigger_c(), 90);
    }

    /// A trigger lowered to 85 °C releases at 79 °C, not at the old fixed 92 °C (which re-engaged every ~35 s).
    #[test]
    fn a_lowered_trigger_doesnt_oscillate() {
        let mut r = EmergencyRule::new(85);
        for t in 0..=5 {
            r.push(t * 1000, 86);
        }
        assert!(r.active());
        for t in 6..=120 {
            assert_eq!(r.push(t * 1000, 86), None, "86 °C holds the boost at t={t}");
        }
        assert!(r.active());
        r.push(121_000, 79);
        assert_eq!(r.push(151_000, 79), Some(Emergency::Release));
    }

    /// R-10: on the fault trace the rule holds full boost almost the whole time, as expected with a dead fan.
    #[test]
    fn on_the_fault_trace_the_emergency_rule_stays_engaged() {
        let mut r = EmergencyRule::new(98);
        let s = f2();
        let engaged = s.iter().filter(|x| { r.push(x.t_ms, x.hottest().unwrap()); r.active() }).count();
        assert!(engaged >= 520, "{engaged} of {}", s.len());
    }

    /// C-10: five failures in a row trip; a success in between resets the count.
    #[test]
    fn the_watchdog_trips_on_five_consecutive_failures() {
        let mut w = Watchdog::default();
        assert!(!(0..4).any(|_| w.write_result(false)));
        assert!(!w.write_result(true));
        assert!(!(0..4).any(|_| w.write_result(false)));
        assert!(w.write_result(false));
        assert!(w.write_result(false), "a failed restore keeps the watchdog tripped");
    }

    /// R-18: no temperature for 30 s is stale.
    #[test]
    fn the_watchdog_sees_stale_readings() {
        let mut w = Watchdog::default();
        assert!(!w.stale(29_999, 0));
        assert!(w.stale(30_000, 0));
        w.temperature_read(40_000);
        assert!(!w.stale(69_999, 0));
        assert!(w.stale(70_000, 0));
    }

    /// C-11: the sixth write in a second is refused; restore writes (five of them) are exempt.
    #[test]
    fn the_rate_limit_allows_five_a_second_and_never_blocks_a_restore() {
        let mut r = RateLimit::default();
        assert!((0..5).all(|i| r.try_write(1_000 + i * 10, false)));
        assert!(!r.try_write(1_100, false));
        assert!((0..5).all(|i| r.try_write(1_100 + i, true)), "watchdog / emergency");
        assert!(r.try_write(2_000, false), "a second later");
    }

    /// R-20: hot, watched or busy reads every second; quiet reads every 10 s.
    #[test]
    fn sampling_speeds_up_when_hot_or_watched() {
        assert_eq!(interval_ms(Some(60), false), 10_000);
        assert_eq!(interval_ms(Some(79), false), 10_000);
        assert_eq!(interval_ms(Some(80), false), 1_000);
        assert_eq!(interval_ms(Some(60), true), 1_000);
        assert_eq!(interval_ms(None, false), 1_000, "no reading yet: look often");
    }
}
