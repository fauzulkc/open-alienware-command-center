//! The pipe end to end on Windows, with the replay firmware (no admin, no hardware): the handshake and
//! readings, refusal of other users and of bad frames, the server identity check, and name squatting.
#![cfg(windows)]

use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wiregent_hw::replay::Replay;
use wiregent_hw_service::controller::{Controller, Shared};
use wiregent_hw_service::identity::{client_images, sddl, Policy};
use wiregent_hw::client::request;
use wiregent_hw_service::pipe::{self, ServerConfig};
use wiregent_hw_service::proto::{self, Request, Response};
use wiregent_hw_service::Answers;

fn me() -> String {
    std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned()
}

/// A test server. `finish` stops it and fails the test if `serve` returned an error; dropping it without
/// `finish` (a test that already failed) only stops it.
struct Server {
    name: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl Server {
    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let r = self.thread.take().unwrap().join().expect("the server thread panicked");
        if let Err(e) = r {
            panic!("server {}: {e:#}", self.name);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Tests run in parallel threads of one process: a process-wide counter keeps every pipe name unique (a
/// clock's nanoseconds don't), and the process id keeps concurrent runs apart.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Starts a server on a fresh pipe name.
fn server(owner_sid: &str) -> Server {
    let name = format!(r"\\.\pipe\wiregent-hw-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
    let shared = Arc::new(Shared::default());
    let mut c = Controller::new(Replay::october(), shared.clone()).unwrap();
    c.step(1_000);
    let mut images = client_images();
    images.push(me());
    let policy = Policy { owner_sid: owner_sid.to_string(), dirs: vec![], any_dir: true, images };
    let cfg = ServerConfig { name: name.clone(), sddl: sddl(&pipe::current_sid().unwrap()).unwrap(), policy, max_instances: 4 };
    let answers = Arc::new(Answers::new(shared, Box::new(|| serde_json::json!({ "test": true })), Box::new(|| 1_500)));
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::spawn(move || {
        let a = answers.clone();
        pipe::serve(cfg, Arc::new(move |c, r| a.answer(c, r)), s, Arc::new(|m: String| eprintln!("{m}")))
    });
    wait_for(Server { name, stop, thread: Some(thread) })
}

/// Waits for the pipe to come up. If this test's server already ended (it couldn't take the name), the
/// test fails with its error rather than talking to whoever holds the name.
fn wait_for(mut server: Server) -> Server {
    let w: Vec<u16> = server.name.encode_utf16().chain(Some(0)).collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if server.thread.as_ref().is_some_and(|t| t.is_finished()) {
            let r = server.thread.take().unwrap().join();
            panic!("server {} ended before it came up: {:?}", server.name, r.map(|r| r.map_err(|e| format!("{e:#}"))));
        }
        if unsafe { windows::Win32::System::Pipes::WaitNamedPipeW(windows::core::PCWSTR(w.as_ptr()), 100) }.as_bool() {
            return server;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{} never came up", server.name);
}

#[test]
fn hello_snapshot_and_lists_over_the_pipe() {
    let srv = server(&pipe::current_sid().unwrap());
    let name = &srv.name;
    let r = request(name, &Request::Snapshot, &me()).unwrap();
    let Response::Snapshot { snapshot: Some(s), stale_ms } = r else { panic!("{r:?}") };
    assert_eq!(s["profile_name"], "G-Mode");
    assert_eq!(stale_ms, Some(500));
    let r = request(name, &Request::Lists, &me()).unwrap();
    let Response::Lists { lists } = r else { panic!("{r:?}") };
    assert_eq!(lists["fans"][2], "0x3B");
    assert!(matches!(request(name, &Request::Health, &me()).unwrap(), Response::Health { .. }));
    srv.finish();
}

/// C-17: a client that isn't the owner gets "not allowed" and nothing else, every time (the refusal must
/// reach the client before the server hangs up).
#[test]
fn a_client_that_isnt_the_owner_is_refused() {
    let srv = server("S-1-5-21-1-2-3-4242");
    for i in 0..20 {
        let err = request(&srv.name, &Request::Snapshot, &me()).unwrap_err();
        assert!(format!("{err:#}").contains(r#"Error { message: "not allowed" }"#), "attempt {i}: {err:#}");
    }
    srv.finish();
}

/// The client refuses a server that isn't the hardware service (here: this test program).
#[test]
fn the_client_checks_who_serves_the_pipe() {
    let srv = server(&pipe::current_sid().unwrap());
    let err = request(&srv.name, &Request::Snapshot, "wiregent-hw.exe").unwrap_err();
    assert!(format!("{err:#}").contains("refusing to talk"), "{err:#}");
    srv.finish();
}

/// C-16 over the wire: an oversized frame header is answered with an error and the connection closed, and
/// the server goes on answering the next client.
#[test]
fn an_oversized_frame_is_refused_on_the_wire() {
    let srv = server(&pipe::current_sid().unwrap());
    let w: Vec<u16> = srv.name.encode_utf16().chain(Some(0)).collect();
    let h = unsafe {
        windows::Win32::Storage::FileSystem::CreateFileW(
            windows::core::PCWSTR(w.as_ptr()),
            windows::Win32::Foundation::GENERIC_READ.0 | windows::Win32::Foundation::GENERIC_WRITE.0,
            windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
            None,
            windows::Win32::Storage::FileSystem::OPEN_EXISTING,
            Default::default(),
            None,
        )
    }
    .unwrap();
    let mut f = unsafe { std::fs::File::from_raw_handle(h.0) };
    f.write_all(&((proto::MAX_FRAME as u32) + 1).to_le_bytes()).unwrap();
    let body = proto::read_frame(&mut f).unwrap();
    let r: Response = serde_json::from_slice(&body).unwrap();
    assert!(matches!(r, Response::Error { .. }), "{r:?}");
    let mut rest = Vec::new();
    let _ = f.read_to_end(&mut rest);
    assert!(rest.is_empty(), "the server closes after a bad frame");
    drop(f);
    let r = request(&srv.name, &Request::Snapshot, &me()).unwrap();
    assert!(matches!(r, Response::Snapshot { snapshot: Some(_), .. }), "a fresh request after the bad frame: {r:?}");
    srv.finish();
}

/// A second server can't take a name that is already served (FILE_FLAG_FIRST_PIPE_INSTANCE).
#[test]
fn a_second_server_cant_squat_the_name() {
    let sid = pipe::current_sid().unwrap();
    let srv = server(&sid);
    let cfg = ServerConfig { name: srv.name.clone(), sddl: sddl(&sid).unwrap(), policy: Policy { owner_sid: sid, dirs: vec![], any_dir: true, images: client_images() }, max_instances: 4 };
    let err = pipe::serve(cfg, Arc::new(|_, _| Response::Error { message: "squatter".into() }), Arc::new(AtomicBool::new(false)), Arc::new(|_m: String| {})).unwrap_err();
    assert!(format!("{err:#}").contains("another process may hold the name"), "{err:#}");
    srv.finish();
}
