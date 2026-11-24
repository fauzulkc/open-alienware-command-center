//! Sending reports: the one place this crate talks to a lighting device.
//!
//! - [`LightDevice`]: the four HID calls a write needs (set/get feature for v5, set output / get input for
//!   v4). [`HidDevice`] is the Windows implementation (the send path proven by `examples/write_test.rs`);
//!   [`FakeDevice`] records what it is sent and fails on demand, for tests anywhere.
//! - [`run_steps`]: carries out a [`Step`] sequence from the builders. Every report is checked with
//!   [`allowed`] first and anything [`is_memory_write`] matches is refused, before a single byte is sent.
//! - [`Pauser`] / [`PauseGuard`]: AWCC's lighting programs suspended around a write (one writer at a time),
//!   resumed when the guard drops, also on an error or a panic. [`AwccPauser`] does it on Windows
//!   (`NtSuspendProcess` / `NtResumeProcess`, no admin needed); [`FakePauser`] counts, for tests.
//!
//! This is the only module besides `probe` that uses `unsafe`, and only in its Windows parts.

use crate::{allowed, is_memory_write, v4, v5, Api, Step};
use std::sync::{Arc, Mutex};

/// The HID calls a lighting write needs. Buffers are whole reports, report id first.
pub trait LightDevice: Send {
    /// v5: `HidD_SetFeature`.
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String>;
    /// v5: `HidD_GetFeature`; `buf[0]` holds the report id on entry.
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<(), String>;
    /// v4: `HidD_SetOutputReport`.
    fn send_output(&mut self, report: &[u8]) -> Result<(), String>;
    /// v4: `HidD_GetInputReport`; `buf[0]` holds the report id on entry.
    fn get_input(&mut self, buf: &mut [u8]) -> Result<(), String>;
    /// The wait between status polls. A fake skips it.
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// How long [`run_steps`] waits for a v4 controller to be ready before giving up (nothing more is sent).
pub const READY_LIMIT_MS: u64 = 2_000;

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunLog {
    /// Reports sent.
    pub sent: usize,
    /// Status bytes read (`ReadStatus` steps), in order.
    pub statuses: Vec<u8>,
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Why `report` may not be sent to an `api` device, if it may not.
pub fn refusal(report: &[u8], api: Api) -> Option<String> {
    if !allowed(report, api) {
        return Some(format!("refused by the allow-list: {}", hex(report)));
    }
    if is_memory_write(report, api) {
        return Some(format!("refused: a controller-memory write: {}", hex(report)));
    }
    None
}

/// Checks every report of `steps` against the allow-list and the memory-write rule.
pub fn check_steps<const N: usize>(api: Api, steps: &[Step<N>]) -> Result<(), String> {
    if N != api.report_len() {
        return Err(format!("{N}-byte reports for a {}-byte device", api.report_len()));
    }
    for s in steps {
        if let Some(r) = s.report() {
            if let Some(why) = refusal(r.as_bytes(), api) {
                return Err(why);
            }
        }
    }
    Ok(())
}

fn v4_status(dev: &mut dyn LightDevice) -> Result<u8, String> {
    let mut buf = [0u8; v4::REPORT_LEN];
    buf[0] = v4::REPORT_ID;
    dev.get_input(&mut buf)?;
    v4::status_byte(&buf).ok_or_else(|| "no status byte".into())
}

/// Carries out `steps` on `dev`, a device speaking `api`:
/// - `Write`: v4 output report / v5 feature report.
/// - `WaitReady` (v4): polls the status every 20 ms for up to 2 s; if it never reads ready, nothing more is
///   sent and the run fails.
/// - `WaitBusy` (v4): polls until busy, at most 500 times, as alien-tools does; not an error if it never is.
/// - `ReadStatus`: reads the status (v5 feature report 0xCC byte 2, v4 input report byte 2).
///
/// Every report is checked first ([`check_steps`]); one refusal and nothing at all is sent.
pub fn run_steps<const N: usize>(dev: &mut dyn LightDevice, api: Api, steps: &[Step<N>]) -> Result<RunLog, String> {
    check_steps(api, steps)?;
    let mut log = RunLog::default();
    for s in steps {
        match (s, api) {
            (Step::Write(r), Api::V4) => dev.send_output(r.as_bytes()),
            (Step::Write(r), Api::V5) => dev.send_feature(r.as_bytes()),
            (Step::WaitReady, Api::V4) => {
                let polls = READY_LIMIT_MS / v4::POLL_INTERVAL_MS as u64;
                let mut ready = false;
                for i in 0..polls {
                    if !matches!(v4::readiness(v4_status(dev)?), v4::Readiness::NotReady) {
                        ready = true;
                        break;
                    }
                    if i + 1 < polls {
                        dev.sleep_ms(v4::POLL_INTERVAL_MS as u64);
                    }
                }
                if ready {
                    Ok(())
                } else {
                    Err(format!("the zone controller never reported ready ({} s); nothing more was sent", READY_LIMIT_MS / 1_000))
                }
            }
            (Step::WaitBusy, Api::V4) => {
                for _ in 0..v4::BUSY_POLL_LIMIT {
                    if v4_status(dev)? == v4::STATUS_BUSY {
                        break;
                    }
                    dev.sleep_ms(v4::POLL_INTERVAL_MS as u64);
                }
                Ok(())
            }
            (Step::ReadStatus, Api::V5) => {
                let mut buf = [0u8; v5::REPORT_LEN];
                buf[0] = v5::REPORT_ID;
                dev.get_feature(&mut buf).map(|()| log.statuses.extend(v5::status_byte(&buf)))
            }
            (Step::ReadStatus, Api::V4) => v4_status(dev).map(|s| log.statuses.push(s)),
            (other, Api::V5) => Err(format!("{other:?} is not a v5 step")),
        }
        .map_err(|e| format!("{e} (after {} report(s) sent)", log.sent))?;
        if s.report().is_some() {
            log.sent += 1;
        }
    }
    Ok(log)
}

// ---------------------------------------------------------------------------------------------------------
// The fake device (tests, dry runs).

/// One call a [`FakeDevice`] received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeCall {
    SetFeature(Vec<u8>),
    GetFeature,
    SetOutput(Vec<u8>),
    GetInput,
}

#[derive(Debug)]
struct FakeState {
    calls: Vec<FakeCall>,
    fail_at: Option<usize>,
    status: u8,
    writes: usize,
}

/// A device that records every call. Clones share one record, so a test keeps a clone and hands the other
/// out. Status reads answer [`FakeDevice::set_status`] (default 33, v4 ready).
#[derive(Clone, Debug)]
pub struct FakeDevice {
    state: Arc<Mutex<FakeState>>,
}

impl Default for FakeDevice {
    fn default() -> Self {
        FakeDevice { state: Arc::new(Mutex::new(FakeState { calls: Vec::new(), fail_at: None, status: v4::STATUS_READY, writes: 0 })) }
    }
}

impl FakeDevice {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `n`-th report sent from now on (0-based, counting every send) fails.
    pub fn fail_at(&self, n: usize) {
        let mut s = self.state.lock().unwrap();
        s.fail_at = Some(s.writes + n);
    }

    /// What status reads answer from now on.
    pub fn set_status(&self, status: u8) {
        self.state.lock().unwrap().status = status;
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// The reports sent, in order (feature and output alike).
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeCall::SetFeature(b) | FakeCall::SetOutput(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    pub fn clear(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    fn write(&self, call: FakeCall) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        let n = s.writes;
        s.writes += 1;
        if s.fail_at == Some(n) {
            return Err("the fake device failed on demand".into());
        }
        s.calls.push(call);
        Ok(())
    }

    fn read(&self, call: FakeCall, buf: &mut [u8]) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        s.calls.push(call);
        if let Some(b) = buf.get_mut(2) {
            *b = s.status;
        }
        Ok(())
    }
}

impl LightDevice for FakeDevice {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
        self.write(FakeCall::SetFeature(report.to_vec()))
    }
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<(), String> {
        self.read(FakeCall::GetFeature, buf)
    }
    fn send_output(&mut self, report: &[u8]) -> Result<(), String> {
        self.write(FakeCall::SetOutput(report.to_vec()))
    }
    fn get_input(&mut self, buf: &mut [u8]) -> Result<(), String> {
        self.read(FakeCall::GetInput, buf)
    }
    fn sleep_ms(&mut self, _ms: u64) {}
}

impl LightDevice for Box<dyn LightDevice> {
    fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
        (**self).send_feature(report)
    }
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<(), String> {
        (**self).get_feature(buf)
    }
    fn send_output(&mut self, report: &[u8]) -> Result<(), String> {
        (**self).send_output(report)
    }
    fn get_input(&mut self, buf: &mut [u8]) -> Result<(), String> {
        (**self).get_input(buf)
    }
    fn sleep_ms(&mut self, ms: u64) {
        (**self).sleep_ms(ms)
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pausing AWCC around a write.

/// Programs are paused while this lives; dropping it resumes them (also while unwinding from a panic).
pub struct PauseGuard {
    /// What was paused ("AlienFXSubAgent.exe pid 1234").
    pub paused: Vec<String>,
    /// What could not be paused (the write goes ahead anyway).
    pub notes: Vec<String>,
    resume: Option<Box<dyn FnOnce() + Send>>,
}

impl PauseGuard {
    /// A guard that runs `resume` when dropped.
    pub fn new(paused: Vec<String>, notes: Vec<String>, resume: impl FnOnce() + Send + 'static) -> Self {
        PauseGuard { paused, notes, resume: Some(Box::new(resume)) }
    }

    /// Nothing paused, nothing to resume.
    pub fn none() -> Self {
        PauseGuard { paused: Vec::new(), notes: Vec::new(), resume: None }
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        if let Some(f) = self.resume.take() {
            f();
        }
    }
}

impl std::fmt::Debug for PauseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PauseGuard").field("paused", &self.paused).field("notes", &self.notes).finish()
    }
}

/// Something that can pause the other lighting writer.
pub trait Pauser: Send + Sync {
    fn pause(&self) -> Result<PauseGuard, String>;

    /// Crash recovery, at app start and before a command-line write: resumes the programs once each,
    /// unconditionally. This covers a crash while a pause (the app's 2 s lease, or a write) held them: a
    /// guard that never dropped never resumed them. A program that isn't suspended is unaffected (its
    /// suspend count is already 0). Exactly one resume per program, never a loop, so a pause another tool
    /// holds on purpose is not undone beyond its own count. Returns what was resumed.
    fn recover(&self) -> Vec<String>;
}

/// AWCC's two lighting programs.
pub const AWCC_LIGHTING: [&str; 2] = ["AlienFXSubAgent.exe", "FxDisplayService.exe"];

/// Suspends AWCC's lighting programs ([`AWCC_LIGHTING`]) on Windows; elsewhere there is nothing to pause.
#[derive(Clone, Copy, Debug, Default)]
pub struct AwccPauser;

impl Pauser for AwccPauser {
    fn pause(&self) -> Result<PauseGuard, String> {
        #[cfg(windows)]
        {
            win::awcc_pause()
        }
        #[cfg(not(windows))]
        {
            Ok(PauseGuard::none())
        }
    }

    fn recover(&self) -> Vec<String> {
        #[cfg(windows)]
        {
            win::awcc_resume_once()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

/// Counts pauses, resumes and recoveries; can refuse to pause.
#[derive(Clone, Debug, Default)]
pub struct FakePauser {
    counts: Arc<Mutex<(u32, u32)>>,
    fail: Arc<Mutex<bool>>,
    recovered: Arc<Mutex<u32>>,
}

impl FakePauser {
    /// How many times [`Pauser::recover`] ran.
    pub fn recovered(&self) -> u32 {
        *self.recovered.lock().unwrap()
    }

    pub fn paused(&self) -> u32 {
        self.counts.lock().unwrap().0
    }
    pub fn resumed(&self) -> u32 {
        self.counts.lock().unwrap().1
    }
    /// Pausing fails from now on.
    pub fn fail(&self) {
        *self.fail.lock().unwrap() = true;
    }
}

impl Pauser for FakePauser {
    fn pause(&self) -> Result<PauseGuard, String> {
        if *self.fail.lock().unwrap() {
            return Err("the fake pauser failed on demand".into());
        }
        self.counts.lock().unwrap().0 += 1;
        let counts = self.counts.clone();
        Ok(PauseGuard::new(vec!["fake".into()], Vec::new(), move || {
            if let Ok(mut c) = counts.lock() {
                c.1 += 1;
            }
        }))
    }

    fn recover(&self) -> Vec<String> {
        *self.recovered.lock().unwrap() += 1;
        vec!["fake".into()]
    }
}

#[cfg(windows)]
pub use win::{awcc_pause, awcc_resume_once, HidDevice};

#[cfg(windows)]
mod win {
    use super::{LightDevice, PauseGuard, AWCC_LIGHTING};
    use crate::probe::{probe, HidInterface};
    use crate::ApiVersion;
    use windows::core::PCWSTR;
    use windows::Win32::Devices::HumanInterfaceDevice::{HidD_GetFeature, HidD_GetInputReport, HidD_SetFeature, HidD_SetOutputReport};
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SUSPEND_RESUME};

    windows::core::link!("ntdll.dll" "system" fn NtSuspendProcess(process : HANDLE) -> i32);
    windows::core::link!("ntdll.dll" "system" fn NtResumeProcess(process : HANDLE) -> i32);

    fn os_error(what: &str) -> String {
        format!("{what}: {}", std::io::Error::last_os_error())
    }

    /// A lighting interface opened for reading and writing (`CreateFileW` with GENERIC_READ | GENERIC_WRITE,
    /// as alien-tools and the L0 test do). Closed on drop.
    pub struct HidDevice {
        handle: HANDLE,
        pub path: String,
        pub vid: u16,
        pub pid: u16,
    }

    // SAFETY: the handle is a plain kernel file handle, usable from any thread; it is only used through
    // `&mut self`.
    unsafe impl Send for HidDevice {}

    impl HidDevice {
        /// The first interface the read-only probe classifies as `api` (V5: the keyboard's 0xCC collection,
        /// V4: the zone controller). `Ok(None)` when there is none.
        pub fn find(api: ApiVersion) -> Result<Option<HidInterface>, String> {
            Ok(probe()?.interfaces.into_iter().find(|i| i.api == Some(api) && i.error.is_none()))
        }

        /// Opens the interface the probe found for `api`.
        pub fn open(api: ApiVersion) -> Result<HidDevice, String> {
            let i = Self::find(api)?.ok_or_else(|| format!("no API {api:?} lighting interface found"))?;
            let w: Vec<u16> = i.path.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `w` is NUL-terminated and outlives the call.
            let handle = unsafe { CreateFileW(PCWSTR(w.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }
                .map_err(|e| format!("open {}: {e}", i.path))?;
            Ok(HidDevice { handle, path: i.path, vid: i.vid, pid: i.pid })
        }
    }

    impl Drop for HidDevice {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateFileW and is closed once.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    impl LightDevice for HidDevice {
        fn send_feature(&mut self, report: &[u8]) -> Result<(), String> {
            // SAFETY: valid handle; the buffer is the whole report and outlives the call.
            unsafe { HidD_SetFeature(self.handle, report.as_ptr().cast(), report.len() as u32) }.then_some(()).ok_or_else(|| os_error(&format!("send {}", super::hex(report))))
        }
        fn get_feature(&mut self, buf: &mut [u8]) -> Result<(), String> {
            // SAFETY: valid handle; `buf` is writable for its whole length.
            unsafe { HidD_GetFeature(self.handle, buf.as_mut_ptr().cast(), buf.len() as u32) }.then_some(()).ok_or_else(|| os_error("read status"))
        }
        fn send_output(&mut self, report: &[u8]) -> Result<(), String> {
            // SAFETY: valid handle; the buffer is the whole report and outlives the call.
            unsafe { HidD_SetOutputReport(self.handle, report.as_ptr().cast(), report.len() as u32) }.then_some(()).ok_or_else(|| os_error(&format!("send {}", super::hex(report))))
        }
        fn get_input(&mut self, buf: &mut [u8]) -> Result<(), String> {
            // SAFETY: valid handle; `buf` is writable for its whole length.
            unsafe { HidD_GetInputReport(self.handle, buf.as_mut_ptr().cast(), buf.len() as u32) }.then_some(()).ok_or_else(|| os_error("read status"))
        }
    }

    /// Process ids of the running programs named in `names` (case-insensitive exe names).
    fn find_processes(names: &[&str]) -> Result<Vec<(String, u32)>, String> {
        // SAFETY: a process snapshot, closed below.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|e| format!("process list: {e}"))?;
        let mut out = Vec::new();
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        // SAFETY: `e.dwSize` is set; the snapshot handle is valid until closed.
        let mut more = unsafe { Process32FirstW(snap, &mut e) }.is_ok();
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
            if names.iter().any(|n| n.eq_ignore_ascii_case(&exe)) {
                out.push((exe, e.th32ProcessID));
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snap, &mut e) }.is_ok();
        }
        // SAFETY: closed once.
        unsafe {
            let _ = CloseHandle(snap);
        }
        Ok(out)
    }

    /// [`super::Pauser::recover`] for AWCC: one `NtResumeProcess` per running lighting program, no loop.
    pub fn awcc_resume_once() -> Vec<String> {
        let mut out = Vec::new();
        for (exe, pid) in find_processes(&AWCC_LIGHTING).unwrap_or_default() {
            // SAFETY: OpenProcess with the one right NtResumeProcess needs.
            if let Ok(h) = unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, false, pid) } {
                // SAFETY: valid handle with PROCESS_SUSPEND_RESUME; resumed once, closed once.
                unsafe {
                    let _ = NtResumeProcess(h);
                    let _ = CloseHandle(h);
                }
                out.push(format!("{exe} pid {pid}"));
            }
        }
        out
    }

    /// Suspends AWCC's lighting programs (`AlienFXSubAgent`, `FxDisplayService`) with `NtSuspendProcess`; the
    /// guard resumes exactly the processes it suspended (same handles) when it drops. A program that isn't
    /// running or can't be opened is noted and skipped: the write goes ahead.
    pub fn awcc_pause() -> Result<PauseGuard, String> {
        let mut held: Vec<(String, isize)> = Vec::new();
        let mut notes = Vec::new();
        for (exe, pid) in find_processes(&AWCC_LIGHTING)? {
            // SAFETY: OpenProcess with the one right NtSuspendProcess needs.
            match unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, false, pid) } {
                Ok(h) => {
                    // SAFETY: `h` is a valid process handle with PROCESS_SUSPEND_RESUME.
                    let status = unsafe { NtSuspendProcess(h) };
                    if status >= 0 {
                        held.push((format!("{exe} pid {pid}"), h.0 as isize));
                    } else {
                        notes.push(format!("{exe} pid {pid}: suspend failed (0x{:08X})", status as u32));
                        // SAFETY: closed once.
                        unsafe {
                            let _ = CloseHandle(h);
                        }
                    }
                }
                Err(e) => notes.push(format!("{exe} pid {pid}: {e}")),
            }
        }
        if held.is_empty() {
            return Ok(PauseGuard { paused: Vec::new(), notes, resume: None });
        }
        let paused = held.iter().map(|(n, _)| n.clone()).collect();
        Ok(PauseGuard::new(paused, notes, move || {
            for (_, h) in held {
                let h = HANDLE(h as *mut core::ffi::c_void);
                // SAFETY: the handle was opened above with PROCESS_SUSPEND_RESUME and suspended once; it is
                // resumed once and closed once.
                unsafe {
                    let _ = NtResumeProcess(h);
                    let _ = CloseHandle(h);
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v4::V4Step;
    use crate::v5::V5Step;
    use crate::Rgb;

    #[test]
    fn v5_steps_send_feature_reports_and_read_status() {
        let dev = FakeDevice::new();
        dev.set_status(0x8c);
        let steps = v5::apply_keys(&[v5::Key::new(0, Rgb::new(255, 0, 0))]).unwrap();
        let log = run_steps(&mut dev.clone(), Api::V5, &steps).unwrap();
        let want: Vec<Vec<u8>> = steps.iter().filter_map(|s| s.report().map(|r| r.as_bytes().to_vec())).collect();
        assert_eq!(dev.sent(), want, "byte for byte, in order");
        assert_eq!(log, RunLog { sent: want.len(), statuses: vec![0x8c] });
        let calls = dev.calls();
        assert_eq!(calls[0], FakeCall::SetFeature(v5::reset_report().as_bytes().to_vec()));
        assert_eq!(calls[2], FakeCall::GetFeature, "the status read follows the status request");
        assert!(calls.iter().all(|c| !matches!(c, FakeCall::SetOutput(_) | FakeCall::GetInput)));
    }

    #[test]
    fn v4_steps_wait_for_ready_and_send_output_reports() {
        let dev = FakeDevice::new();
        let mut steps = v4::reset();
        steps.push(Step::Write(v4::set_one_color(1, Rgb::new(0, 0, 255))));
        steps.push(Step::Write(v4::update()));
        let log = run_steps(&mut dev.clone(), Api::V4, &steps).unwrap();
        assert_eq!(log.sent, 4);
        assert_eq!(dev.calls()[0], FakeCall::GetInput, "ready checked before the first report");
        assert!(matches!(dev.calls()[1], FakeCall::SetOutput(_)));
    }

    #[test]
    fn not_ready_in_two_seconds_sends_nothing() {
        let dev = FakeDevice::new();
        dev.set_status(v4::STATUS_BUSY);
        let mut steps = v4::reset();
        steps.push(Step::Write(v4::update()));
        let e = run_steps(&mut dev.clone(), Api::V4, &steps).unwrap_err();
        assert!(e.contains("never reported ready"), "{e}");
        assert!(dev.sent().is_empty());
        assert_eq!(dev.calls().len(), 100, "2 s of 20 ms polls");
    }

    #[test]
    fn refused_reports_stop_the_whole_run_before_anything_is_sent() {
        let dev = FakeDevice::new();
        // A v4 power-slot write: allowed shape, but a controller-memory write.
        let steps: Vec<V4Step> = v4::set_power_action(1, &[v4::Action::color(Rgb::new(1, 2, 3))]).unwrap();
        let e = run_steps(&mut dev.clone(), Api::V4, &steps).unwrap_err();
        assert!(e.contains("controller-memory write"), "{e}");
        assert!(dev.calls().is_empty(), "not even a status poll");
        // A v5 sequence to a v4 device: wrong length.
        let e = run_steps(&mut dev.clone(), Api::V4, &v5::reset()).unwrap_err();
        assert!(e.contains("64-byte reports"), "{e}");
        assert!(dev.calls().is_empty());
        assert!(refusal(&[0xcc, 0x99], Api::V5).is_some());
    }

    #[test]
    fn a_failed_send_stops_the_run() {
        let dev = FakeDevice::new();
        dev.fail_at(1);
        let steps: Vec<V5Step> = v5::brightness(10);
        let e = run_steps(&mut dev.clone(), Api::V5, &steps).unwrap_err();
        assert!(e.contains("failed on demand") && e.contains("after 1 report(s) sent"), "{e}");
        assert_eq!(dev.sent().len(), 1);
    }

    #[test]
    fn the_pause_guard_resumes_on_drop_and_on_panic() {
        let p = FakePauser::default();
        {
            let _g = p.pause().unwrap();
            assert_eq!((p.paused(), p.resumed()), (1, 0));
        }
        assert_eq!((p.paused(), p.resumed()), (1, 1));
        let p2 = p.clone();
        let r = std::panic::catch_unwind(move || {
            let _g = p2.pause().unwrap();
            panic!("boom");
        });
        assert!(r.is_err());
        assert_eq!((p.paused(), p.resumed()), (2, 2), "resumed while unwinding");
        p.fail();
        assert!(p.pause().is_err());
        drop(PauseGuard::none());
        assert_eq!(p.recover(), vec!["fake".to_string()]);
        assert_eq!(p.recovered(), 1, "one resume per call, no loop");
    }
}
