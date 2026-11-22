//! `wiregent-hw`: the Wiregent hardware service (docs/plans/x17-hw-service-design.md). H1b: read-only.
//!
//!   wiregent-hw                     run as the Windows service "wiregent-hw" (installed by the app's hw-install op)
//!   wiregent-hw --console [--replay] [--pipe NAME] [--owner-sid SID] [--any-dir]
//!                                   run in this terminal until Enter is pressed (development and tests).
//!                                   --replay answers from the 2026-10-07 capture instead of the firmware.
//!
//! Service mode reads its owner from C:\ProgramData\Wiregent\hw\owner.json, which the install op writes. It
//! writes no files in H1b: messages go to the debugger log until the locked-down data folder exists.

#[cfg(not(windows))]
fn main() {
    eprintln!("wiregent-hw runs on Windows only");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--console") {
        if let Err(e) = win::console(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    } else if let Err(e) = win::dispatch() {
        eprintln!("wiregent-hw is a Windows service; for a terminal run use --console ({e})");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use anyhow::{Context, Result};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, OnceLock};
    use std::time::Instant;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Services::*;
    use windows::Win32::UI::WindowsAndMessaging::{PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND};
    use wiregent_hw_service::controller::{watchdog, Controller, Health, Shared};
    use wiregent_hw_service::identity::{sddl, Policy};
    use wiregent_hw_service::pipe::{self, ServerConfig};
    use wiregent_hw_service::{proto, Answers};

    const SERVICE: &str = "wiregent-hw";

    pub struct Options {
        pub pipe: String,
        pub owner_sid: String,
        pub dirs: Vec<PathBuf>,
        pub any_dir: bool,
        pub replay: bool,
    }

    fn log_line(msg: &str) {
        let w: Vec<u16> = format!("wiregent-hw: {msg}\n").encode_utf16().chain(Some(0)).collect();
        unsafe { windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(PCWSTR(w.as_ptr())) };
    }

    /// Runs the controller, the watchdog and the pipe until `shared.stop`.
    pub fn run(opts: Options, shared: Arc<Shared>, log: Arc<dyn Fn(String) + Send + Sync>) -> Result<()> {
        let start = Instant::now();
        let now = move || start.elapsed().as_millis() as u64;
        // Controller: the only thread with a firmware connection (its own COM apartment).
        let (c_shared, c_log, replay) = (shared.clone(), log.clone(), opts.replay);
        std::thread::spawn(move || {
            type Fw = Box<dyn wiregent_hw::control::FirmwareWrite>;
            let built = if replay {
                Controller::with_gate(Box::new(wiregent_hw::replay::Replay::october()) as Fw, c_shared.clone(), true)
            } else {
                let ok = wiregent_hw::control::this_machine_verified();
                wiregent_hw::wmi::Awcc::open().and_then(|fw| Controller::with_gate(Box::new(fw) as Fw, c_shared.clone(), ok))
            };
            match built {
                Ok(c) => c.run(now),
                Err(e) => c_log(format!("firmware unavailable, serving no readings: {e:#}")),
            }
        });
        // Watchdog: reports a stuck controller once per episode (H2 adds the restore on its own connection).
        let (w_shared, w_log) = (shared.clone(), log.clone());
        std::thread::spawn(move || {
            let mut reported = false;
            while !w_shared.stop.load(Ordering::SeqCst) {
                match watchdog(&w_shared, now()) {
                    Health::Stuck { for_ms } if !reported && !w_shared.is_paused() => {
                        w_log(format!("controller stuck for {} s", for_ms / 1000));
                        reported = true;
                    }
                    Health::Ok => reported = false,
                    _ => {}
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
        let answers = Arc::new(Answers::new(shared.clone(), Box::new(|| serde_json::to_value(wiregent_hw::health::read(30)).unwrap_or_default()), Box::new(now)));
        let cfg = ServerConfig { name: opts.pipe, sddl: sddl(&opts.owner_sid).map_err(anyhow::Error::msg)?, policy: Policy { owner_sid: opts.owner_sid, dirs: opts.dirs, any_dir: opts.any_dir, images: wiregent_hw_service::identity::client_images() }, max_instances: 4 };
        let a = answers.clone();
        pipe::serve(cfg, Arc::new(move |c, r| a.answer(c, r)), Arc::new(StopFlag(shared.clone())).flag(), log)
    }

    /// Lets `pipe::serve` watch the shared stop flag.
    struct StopFlag(Arc<Shared>);
    impl StopFlag {
        fn flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (f, s) = (flag.clone(), self.0.clone());
            std::thread::spawn(move || loop {
                if s.stop.load(Ordering::SeqCst) {
                    f.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            });
            flag
        }
    }

    pub fn console(args: &[String]) -> Result<()> {
        let val = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
        let owner = match val("--owner-sid") {
            Some(s) => s,
            None => pipe::current_sid()?,
        };
        let opts = Options { pipe: val("--pipe").unwrap_or_else(|| proto::PIPE_NAME.into()), owner_sid: owner, dirs: Vec::new(), any_dir: true, replay: args.iter().any(|a| a == "--replay") };
        if !opts.replay && !wiregent_hw::is_admin() {
            eprintln!("note: not elevated, so the firmware can't be read; use --replay to serve the October capture");
        }
        eprintln!("serving {} for {} (press Enter to stop; with no keyboard it runs until stopped)", opts.pipe, opts.owner_sid);
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        std::thread::spawn(move || {
            let mut l = String::new();
            // Enter stops it; no keyboard at all (end of input) means "run until killed".
            if matches!(std::io::stdin().read_line(&mut l), Ok(n) if n > 0) {
                s.stop.store(true, Ordering::SeqCst);
            }
        });
        run(opts, shared, Arc::new(|m: String| eprintln!("{m}")))
    }

    // ------------------------------------------------------------------ service mode

    static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
    /// The status handle as an integer: the service manager's handle is valid process-wide.
    static STATUS: OnceLock<isize> = OnceLock::new();

    fn report(state: SERVICE_STATUS_CURRENT_STATE, exit: u32) {
        let Some(&h) = STATUS.get() else { return };
        let h = SERVICE_STATUS_HANDLE(h as *mut _);
        let accepts = if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_POWEREVENT } else { 0 };
        let st = SERVICE_STATUS { dwServiceType: SERVICE_WIN32_OWN_PROCESS, dwCurrentState: state, dwControlsAccepted: accepts, dwWin32ExitCode: exit, dwServiceSpecificExitCode: 0, dwCheckPoint: 0, dwWaitHint: 5_000 };
        unsafe {
            let _ = SetServiceStatus(h, &st);
        }
    }

    unsafe extern "system" fn control(code: u32, event: u32, _data: *mut core::ffi::c_void, _ctx: *mut core::ffi::c_void) -> u32 {
        let shared = SHARED.get();
        match code {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                report(SERVICE_STOP_PENDING, 0);
                if let Some(s) = shared {
                    s.stop.store(true, Ordering::SeqCst);
                }
                0
            }
            SERVICE_CONTROL_POWEREVENT => {
                if let Some(s) = shared {
                    match event {
                        PBT_APMSUSPEND => s.pause(),
                        PBT_APMRESUMEAUTOMATIC => s.resume(),
                        _ => {}
                    }
                }
                0
            }
            SERVICE_CONTROL_INTERROGATE => 0,
            _ => 120, // ERROR_CALL_NOT_IMPLEMENTED
        }
    }

    fn owner_from_install() -> Result<(String, Vec<PathBuf>)> {
        let file = PathBuf::from(r"C:\ProgramData\Wiregent\hw\owner.json");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?)?;
        let sid = v["sid"].as_str().context("owner.json has no sid")?.to_string();
        let dirs = v["appDirs"].as_array().map(|a| a.iter().filter_map(|d| d.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
        Ok((sid, dirs))
    }

    unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
        let name: Vec<u16> = SERVICE.encode_utf16().chain(Some(0)).collect();
        let Ok(h) = RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(control), None) else { return };
        let _ = STATUS.set(h.0 as isize);
        let shared = Arc::new(Shared::default());
        let _ = SHARED.set(shared.clone());
        report(SERVICE_RUNNING, 0);
        let result = owner_from_install().and_then(|(sid, mut dirs)| {
            dirs.push(PathBuf::from(r"C:\Program Files\Wiregent\hw"));
            run(Options { pipe: proto::PIPE_NAME.into(), owner_sid: sid, dirs, any_dir: false, replay: false }, shared, Arc::new(|m: String| log_line(&m)))
        });
        if let Err(e) = &result {
            log_line(&format!("stopped: {e:#}"));
        }
        report(SERVICE_STOPPED, if result.is_ok() { 0 } else { 1066 }); // ERROR_SERVICE_SPECIFIC_ERROR
    }

    pub fn dispatch() -> Result<()> {
        let mut name: Vec<u16> = SERVICE.encode_utf16().chain(Some(0)).collect();
        let table = [SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(name.as_mut_ptr()), lpServiceProc: Some(service_main) }, SERVICE_TABLE_ENTRYW::default()];
        unsafe { StartServiceCtrlDispatcherW(table.as_ptr())? };
        Ok(())
    }
}
