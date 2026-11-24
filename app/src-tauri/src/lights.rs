//! Keyboard lighting in the app (docs/lighting-control.md): writes, the AWCC pause lease, and the power
//! events that darken and restore the keyboard. What a write sends, the settings' rules and the event ->
//! action mapping are cockpit-core's `lightctl`, shared with `cockpit lighting`.
//!
//! Every write goes through [`Runtime`] behind one mutex, so writes never interleave. AWCC's lighting
//! programs are paused with a lease: the first write suspends them, and one timer resumes them 2 s after the
//! last write (the map's live preview sends a write per colour change). The lease is also released on exit,
//! and AWCC resumes if a write panics.
//!
//! The reactive glow's runner lives here too ([`Runtime::glow`], driven by `glow.rs`), so its frames and the
//! app's other writes never interleave: a settings write carries the glow on from the new static colours
//! ([`Glow::Resync`]) or ends it, and AWCC's look written back ends it.
//!
//! The live animation (`live.rs`: the clock, mouse paint, the screen mirror) runs on its own thread with its
//! own writes. Every write here first takes a hold on it ([`LIVE`]): it stops with the static colours written
//! back before the write, and starts again once nothing holds it.

use cockpit_core::lightctl::{self, LastApply, LightIo, LightingSettings, LightingStatus, Mode, PauseLease, PowerState, Shown, SystemIo};
use cockpit_core::reactive::{self, Runner};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What lighting writes share. Holding the lock is what serialises them.
pub struct Runtime {
    pub power: PowerState,
    pub last: Option<LastApply>,
    pub lease: PauseLease,
    /// What the keyboard shows after this app's last write (AWCC's own state until then): writes send only what changes.
    pub shown: Shown,
    /// The reactive glow while it runs (it holds its own AWCC pause for the whole run).
    pub glow: Option<Runner>,
    /// Settings the glow couldn't start or run with: not tried again until they change or a write succeeds.
    pub glow_failed: Option<LightingSettings>,
    /// The app is quitting: the glow doesn't start again.
    pub quitting: bool,
    start: Instant,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime { power: PowerState::default(), last: None, lease: PauseLease::default(), shown: Shown::AWCC, glow: None, glow_failed: None, quitting: false, start: Instant::now() }
    }
}

impl Runtime {
    /// The lease's steady clock, ms (the glow's too).
    pub fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Ends the glow without writing (another write replaces the look, or the keyboard goes dark).
    pub fn end_glow(&mut self) {
        if let Some(r) = self.glow.take() {
            r.abandon();
        }
    }
}

/// What a write does with a running glow.
pub enum Glow<'a> {
    /// Leave it (a one-key preview).
    Keep,
    /// The write puts these static settings on the keyboard: the glow carries on from them when it still
    /// can, else it ends.
    Resync(&'a LightingSettings),
    /// It ends (AWCC's look written back).
    End,
}

pub type Shared = Arc<Mutex<Runtime>>;

/// The live animation's hand-off with the app's other writes. Never wait on it while holding the lighting
/// lock: the live thread takes that lock as it finishes.
pub struct LiveGate {
    /// Set by a hold: the running animation stops (it checks every poll).
    stop: AtomicBool,
    /// An animation is running (its writes may be in flight).
    running: AtomicBool,
    /// Writes waiting or in progress: no animation starts while any.
    holds: AtomicUsize,
}

pub static LIVE: LiveGate = LiveGate { stop: AtomicBool::new(false), running: AtomicBool::new(false), holds: AtomicUsize::new(0) };

/// How long a hold waits for a running animation to stop and put the static colours back.
const HOLD_WAIT: Duration = Duration::from_secs(5);

/// A write's hold on the live animation; released when dropped.
pub struct LiveHold;

impl Drop for LiveHold {
    fn drop(&mut self) {
        LIVE.holds.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LiveGate {
    /// Stops a running animation and keeps it from starting until the hold is dropped. Waits (up to 5 s) for
    /// it to finish its write-back.
    pub fn hold(&'static self) -> LiveHold {
        self.holds.fetch_add(1, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        let t = Instant::now();
        while self.running.load(Ordering::SeqCst) && t.elapsed() < HOLD_WAIT {
            std::thread::sleep(Duration::from_millis(5));
        }
        LiveHold
    }

    /// The live thread, before a run: true when it may start (nothing holds it). The order matters: a hold
    /// that comes in between either sees `running` and waits, or is seen here.
    pub fn try_start(&self) -> bool {
        self.stop.store(false, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        if self.holds.load(Ordering::SeqCst) > 0 {
            self.running.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// The live thread, after a run (and after it updated the runtime).
    pub fn finished(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

/// The lock, also after a panic elsewhere (the lease must still be released).
pub fn lock(s: &Shared) -> MutexGuard<'_, Runtime> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one timer: resumes AWCC once the lease's hold has passed since the last write.
fn start_timer(shared: Shared) {
    std::thread::spawn(move || loop {
        let wait = {
            let mut rt = lock(&shared);
            let now = rt.now();
            rt.lease.tick(now)
        };
        match wait {
            Some(ms) => std::thread::sleep(Duration::from_millis(ms.max(10))),
            None => break,
        }
    });
}

/// Runs one write under the lock, with AWCC paused through the lease when `pause_awcc`, and records it as
/// the last apply. The glow is handled as `glow` says.
fn write_with(shared: &Shared, pause_awcc: bool, glow: Glow, f: impl FnOnce(&mut dyn LightIo, &PowerState, &mut Shown) -> Result<String, String>) -> LastApply {
    // The live animation stops (static colours back) before this write, and not while holding the lock.
    let _live = LIVE.hold();
    let mut rt = lock(shared);
    let runner = match glow {
        Glow::Keep => None,
        _ => rt.glow.take(),
    };
    let mut io = SystemIo;
    let result = (|| -> Result<String, String> {
        if pause_awcc {
            let now = rt.now();
            if rt.lease.acquire(&lightctl::alienfx::device::AwccPauser, now)? {
                start_timer(shared.clone());
            }
        } else {
            rt.lease.release();
        }
        let power = rt.power;
        let mut shown = rt.shown;
        let r = catch_unwind(AssertUnwindSafe(|| f(&mut io, &power, &mut shown)));
        if matches!(r, Ok(Ok(_))) {
            rt.shown = shown;
        }
        match r {
            Ok(r) => r,
            Err(_) => {
                rt.lease.release();
                Err("the write stopped unexpectedly; AWCC was resumed".into())
            }
        }
    })();
    let now = rt.now();
    rt.lease.touch(now);
    if let Some(mut r) = runner {
        let p = rt.power;
        match glow {
            Glow::Resync(s) if result.is_ok() && reactive::wanted(s, &p) && r.resync(s, &p).is_ok() => rt.glow = Some(r),
            _ => r.abandon(),
        }
    }
    if result.is_ok() {
        rt.glow_failed = None;
    }
    let (ok, mut text) = match result {
        Ok(t) => (true, t),
        Err(e) => (false, e),
    };
    if rt.lease.held() {
        text.push_str(" AWCC's lighting programs resume 2 s after the last write.");
    }
    let last = LastApply { ok, at: lightctl::now_ms(), text };
    rt.last = Some(last.clone());
    last
}

/// Writes `s` for the current power state, in custom mode only (`None` in awcc mode: the app never writes).
pub fn apply_settings(shared: &Shared, s: &LightingSettings) -> Option<LastApply> {
    if s.mode != Mode::Custom {
        let _live = LIVE.hold();
        lock(shared).end_glow();
        return None;
    }
    Some(write_with(shared, s.pause_awcc, Glow::Resync(s), |io, p, shown| {
        let a = lightctl::write_from(io, *shown, s, p, None)?;
        *shown = Shown::of(s, p);
        Ok(format!("Applied: {}", a.text()))
    }))
}

/// One key in one colour, not saved (the map's live preview). Custom mode only.
pub fn preview(shared: &Shared, s: &LightingSettings, id: u32, color: &str) -> Result<LastApply, String> {
    if s.mode != Mode::Custom {
        return Err("Lighting is AWCC's (mode awcc), so nothing is written: switch to custom to preview keys.".into());
    }
    let sent = lightctl::rgb_hex(s.led(lightctl::parse_rgb(color)?));
    let steps = lightctl::preview_plan(id, &sent)?;
    Ok(write_with(shared, s.pause_awcc, Glow::Keep, |io, _, _| {
        let mut dev = io.keyboard()?;
        lightctl::apply(dev.as_mut(), &steps, None).map(|(l, _, _)| format!("Preview: key {id} in {color} ({} reports).", l.sent))
    }))
}

/// AWCC's own look written back (the caller has set the mode to awcc).
pub fn restore(shared: &Shared, pause_awcc: bool) -> LastApply {
    write_with(shared, pause_awcc, Glow::End, |io, _, shown| {
        let (look, a) = lightctl::restore_from(io, *shown, None)?;
        *shown = Shown::AWCC;
        Ok(format!("AWCC's \"{}\" is back on the keyboard: {}", look.preset, a.text()))
    })
}

/// On quit: the glow stops with the static colours written back (AWCC resumed after), and nothing starts it
/// again; then any lease still held resumes AWCC.
pub fn quit(shared: &Shared) {
    lock(shared).quitting = true;
    // The live animation stops with the static colours written back.
    let _live = LIVE.hold();
    let mut rt = lock(shared);
    if let Some(r) = rt.glow.take() {
        if let Err(e) = r.stop(&mut SystemIo) {
            eprintln!("reactive glow: the static colours couldn't be written back on quit: {e}");
        }
    }
    rt.lease.release();
}

/// Crash recovery at start: AWCC's lighting programs resumed once each (a crash during the 2 s pause lease
/// would have left them suspended). One resume per program, never a loop.
pub fn recover() -> Vec<String> {
    use lightctl::alienfx::device::Pauser;
    lightctl::alienfx::device::AwccPauser.recover()
}

/// AWCC's look as custom settings (nothing written).
pub fn import(s: &LightingSettings) -> Result<LightingSettings, String> {
    let look = SystemIo.awcc()?.ok_or("AWCC has no active keyboard preset to import")?;
    Ok(lightctl::import_awcc(s, &look))
}

/// The lighting page's data. Also notes whether the keyboard was found, for `controls`.
pub fn status(shared: &Shared, s: &LightingSettings, found: &AtomicBool) -> LightingStatus {
    let mut io = SystemIo;
    let awcc = io.awcc().ok().flatten();
    let device = io.detect();
    found.store(device.keyboard, Ordering::SeqCst);
    let last = lock(shared).last.clone();
    lightctl::status(s, awcc, device, last)
}

/// Power broadcasts (Windows): a hidden top-level window, because broadcasts such as WM_POWERBROADCAST don't
/// reach message-only windows. Suspend and resume come as `PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC`; the
/// lid, the display and Energy Saver through `RegisterPowerSettingNotification`.
#[cfg(windows)]
pub mod win {
    use cockpit_core::lightctl::{self, PowerEvent, PowerSetting};
    use std::cell::RefCell;
    use windows::core::{w, GUID, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{RegisterPowerSettingNotification, POWERBROADCAST_SETTING};
    use windows::Win32::System::SystemServices::{GUID_CONSOLE_DISPLAY_STATE, GUID_LIDSWITCH_STATE_CHANGE};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, DEVICE_NOTIFY_WINDOW_HANDLE, MSG, PBT_POWERSETTINGCHANGE, WINDOW_STYLE, WM_POWERBROADCAST, WNDCLASSW, WS_EX_TOOLWINDOW};

    /// GUID_ENERGY_SAVER_STATUS (winnt.h; not in the windows crate): 0 off, 1 standard, 2 high savings.
    pub const GUID_ENERGY_SAVER_STATUS: GUID = GUID::from_u128(0x550e8400_e29b_41d4_a716_446655440000);

    type OnEvent = Option<Box<dyn FnMut(PowerEvent)>>;

    thread_local! {
        static ON_EVENT: RefCell<OnEvent> = const { RefCell::new(None) };
    }

    fn setting(guid: &GUID) -> Option<PowerSetting> {
        if *guid == GUID_LIDSWITCH_STATE_CHANGE {
            Some(PowerSetting::Lid)
        } else if *guid == GUID_CONSOLE_DISPLAY_STATE {
            Some(PowerSetting::Display)
        } else if *guid == GUID_ENERGY_SAVER_STATUS {
            Some(PowerSetting::EnergySaver)
        } else {
            None
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_POWERBROADCAST {
            let pbt = wp.0 as u32;
            let ev = if pbt == PBT_POWERSETTINGCHANGE && lp.0 != 0 {
                // SAFETY: for PBT_POWERSETTINGCHANGE, lParam points to a POWERBROADCAST_SETTING whose Data holds
                // DataLength bytes (a DWORD for these three settings).
                let s = &*(lp.0 as *const POWERBROADCAST_SETTING);
                let value = if s.DataLength >= 4 { std::ptr::read_unaligned(s.Data.as_ptr() as *const u32) } else { s.Data[0] as u32 };
                setting(&s.PowerSetting).map(|st| lightctl::setting_event(st, value))
            } else {
                lightctl::broadcast_event(pbt)
            };
            if let Some(ev) = ev {
                ON_EVENT.with(|f| {
                    if let Some(f) = f.borrow_mut().as_mut() {
                        f(ev);
                    }
                });
            }
            return LRESULT(1);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// Listens on this thread forever and calls `on_event` for each power change. A suspend is handled
    /// before this returns to Windows, so the keyboard goes dark before the laptop sleeps.
    pub fn listen(on_event: impl FnMut(PowerEvent) + 'static) -> anyhow::Result<()> {
        ON_EVENT.with(|f| *f.borrow_mut() = Some(Box::new(on_event)));
        unsafe {
            let hinst = GetModuleHandleW(None)?;
            let class = w!("AlienCockpitPower");
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() });
            // Top-level (no HWND_MESSAGE parent) and never shown.
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
            for guid in [GUID_LIDSWITCH_STATE_CHANGE, GUID_CONSOLE_DISPLAY_STATE, GUID_ENERGY_SAVER_STATUS] {
                if let Err(e) = RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, DEVICE_NOTIFY_WINDOW_HANDLE) {
                    eprintln!("power notification {guid:?} unavailable: {e}");
                }
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}
