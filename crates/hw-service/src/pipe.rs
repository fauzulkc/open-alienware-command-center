//! The named pipe (design §4). Server: first instance only, no remote clients, owner-only security
//! descriptor, overlapped reads with a 5 s idle timeout, a whole frame within 5 s of its first byte (a client
//! dribbling bytes can't hold a connection), at most `max_instances` connections, and every client identified
//! from its process before it is served. The client side is `wiregent_hw::client`.

use crate::identity::Policy;
use wiregent_hw::client::Client;
use wiregent_hw::proto::{self, Request, Response};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, WAIT_TIMEOUT};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

pub const IDLE_TIMEOUT_MS: u32 = 5_000;
/// Once a frame's first byte arrives, the rest of it (header and body) must arrive within this. The idle
/// timeout alone applies per read, so one byte every few seconds would hold a connection for ever.
pub const FRAME_DEADLINE_MS: u64 = 5_000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub use wiregent_hw::client::{current_sid, process_identity};

struct Io {
    h: HANDLE,
    ov: OVERLAPPED,
}

impl Io {
    fn new(h: HANDLE) -> Result<Self> {
        let ev = unsafe { CreateEventW(None, true, false, PCWSTR::null())? };
        Ok(Io { h, ov: OVERLAPPED { hEvent: ev, ..Default::default() } })
    }

    /// Waits for the pending operation; cancels it after `timeout_ms`.
    fn finish(&mut self, started: windows::core::Result<()>, timeout_ms: u32) -> Result<u32> {
        unsafe {
            if let Err(e) = started {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return Err(anyhow!(e));
                }
                if WaitForSingleObject(self.ov.hEvent, timeout_ms) == WAIT_TIMEOUT {
                    let _ = CancelIoEx(self.h, Some(&self.ov));
                    let mut n = 0;
                    let _ = GetOverlappedResult(self.h, &self.ov, &mut n, true);
                    bail!("timed out");
                }
            }
            let mut n = 0u32;
            GetOverlappedResult(self.h, &self.ov, &mut n, false)?;
            Ok(n)
        }
    }

    fn read_exact(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<()> {
        let mut got = 0;
        while got < buf.len() {
            let r = unsafe { ReadFile(self.h, Some(&mut buf[got..]), None, Some(&mut self.ov)) };
            let n = self.finish(r, timeout_ms)? as usize;
            if n == 0 {
                bail!("the client closed the pipe");
            }
            got += n;
        }
        Ok(())
    }

    /// Reads all of `buf` by `deadline`, however the bytes are spread out.
    fn read_exact_by(&mut self, buf: &mut [u8], deadline: Instant) -> Result<()> {
        let mut got = 0;
        while got < buf.len() {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { bail!("past the deadline") };
            let r = unsafe { ReadFile(self.h, Some(&mut buf[got..]), None, Some(&mut self.ov)) };
            let n = self.finish(r, (left.as_millis() as u32).max(1))? as usize;
            if n == 0 {
                bail!("the client closed the pipe");
            }
            got += n;
        }
        Ok(())
    }

    /// After a final answer: give the client up to a second to read it and close, because disconnecting a
    /// pipe throws away whatever the client hasn't read yet. Whatever the client still sends (such as the
    /// request a refusal never read) is read and dropped; one byte arriving is not the client closing.
    fn linger(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1_000);
        let mut b = [0u8; 1];
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            if self.read_exact(&mut b, (left.as_millis() as u32).max(1)).is_err() {
                break;
            }
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        let mut sent = 0;
        while sent < data.len() {
            let r = unsafe { WriteFile(self.h, Some(&data[sent..]), None, Some(&mut self.ov)) };
            sent += self.finish(r, IDLE_TIMEOUT_MS)? as usize;
        }
        Ok(())
    }
}

impl Drop for Io {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.ov.hEvent);
        }
    }
}

pub type Handler = Arc<dyn Fn(&Client, Request) -> Response + Send + Sync>;

pub struct ServerConfig {
    pub name: String,
    pub sddl: String,
    pub policy: Policy,
    pub max_instances: u32,
}

#[derive(Clone, Copy)]
struct SendHandle(isize);
unsafe impl Send for SendHandle {}

fn serve_client(h: HANDLE, policy: &Policy, handler: &Handler, log: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
    let mut io = Io::new(h)?;
    let mut pid = 0u32;
    unsafe { GetNamedPipeClientProcessId(h, &mut pid)? };
    let client = process_identity(pid)?;
    if let Err(why) = policy.allow(&client) {
        log(format!("refused pipe client: {why}"));
        let _ = io.write_all(&proto::encode(&Response::Error { message: "not allowed".into() })?);
        io.linger();
        return Ok(());
    }
    loop {
        let mut header = [0u8; 4];
        if io.read_exact(&mut header[..1], IDLE_TIMEOUT_MS).is_err() {
            return Ok(()); // idle, or the client left
        }
        // The first byte is in: the whole frame has FRAME_DEADLINE_MS from here.
        let deadline = Instant::now() + Duration::from_millis(FRAME_DEADLINE_MS);
        let too_slow = |e: anyhow::Error| format!("pipe client {} didn't send a whole request within {} s ({e:#}): disconnected", client.pid, FRAME_DEADLINE_MS / 1_000);
        if let Err(e) = io.read_exact_by(&mut header[1..], deadline) {
            log(too_slow(e));
            return Ok(());
        }
        let len = match proto::frame_len(header) {
            Ok(n) => n,
            Err(e) => {
                log(format!("pipe client {} sent a bad frame: {e}", client.pid));
                let _ = io.write_all(&proto::encode(&Response::Error { message: e.to_string() })?);
                io.linger();
                return Ok(());
            }
        };
        let mut body = vec![0u8; len];
        if let Err(e) = io.read_exact_by(&mut body, deadline) {
            log(too_slow(e));
            return Ok(());
        }
        let resp = match proto::parse_request(&body) {
            Ok(req) => handler(&client, req),
            Err(err) => err,
        };
        io.write_all(&proto::encode(&resp).unwrap_or_else(|e| proto::encode(&Response::Error { message: e.to_string() }).expect("small error frame")))?;
    }
}

/// Serves until `stop`. Fails at once when the pipe name is already taken (someone squatting it).
pub fn serve(cfg: ServerConfig, handler: Handler, stop: Arc<AtomicBool>, log: Arc<dyn Fn(String) + Send + Sync>) -> Result<()> {
    let name = wide(&cfg.name);
    let sddl = wide(&cfg.sddl);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None).context("building the pipe's security descriptor")? };
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
    let active = Arc::new(AtomicU32::new(0));
    let policy = Arc::new(cfg.policy);
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        let open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { Default::default() };
        let h = unsafe { CreateNamedPipeW(PCWSTR(name.as_ptr()), open, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, cfg.max_instances, 64 * 1024, 64 * 1024, 0, Some(&sa)) };
        if h.is_invalid() {
            if first {
                let e = windows::core::Error::from_thread();
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(sd.0)));
                }
                bail!("can't create {} ({}): another process may hold the name", cfg.name, e.message());
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        first = false;
        let mut io = Io::new(h)?;
        let started = unsafe { ConnectNamedPipe(h, Some(&mut io.ov)) };
        let connected = match started {
            Ok(()) => true,
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => true,
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => loop {
                if stop.load(Ordering::SeqCst) {
                    unsafe {
                        let _ = CancelIoEx(h, Some(&io.ov));
                    }
                    break false;
                }
                if unsafe { WaitForSingleObject(io.ov.hEvent, 250) } != WAIT_TIMEOUT {
                    break true;
                }
            },
            Err(_) => false,
        };
        drop(io);
        if !connected {
            unsafe {
                let _ = CloseHandle(h);
            }
            continue;
        }
        if active.load(Ordering::SeqCst) >= cfg.max_instances {
            unsafe {
                let _ = DisconnectNamedPipe(h);
                let _ = CloseHandle(h);
            }
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        let (active, policy, handler, log, sh) = (active.clone(), policy.clone(), handler.clone(), log.clone(), SendHandle(h.0 as isize));
        std::thread::spawn(move || {
            let h = HANDLE(sh.0 as *mut _);
            if let Err(e) = serve_client(h, &policy, &handler, &*log) {
                log(format!("pipe client ended: {e:#}"));
            }
            unsafe {
                let _ = DisconnectNamedPipe(h);
                let _ = CloseHandle(h);
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    Ok(())
}

