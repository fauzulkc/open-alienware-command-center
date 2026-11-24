//! The hardware service's pipe, client side (design §4): connect identification-only (the service can never
//! impersonate the caller), check that the server is the hardware service, say hello, ask one thing.
//!
//! "The server is the hardware service" (C-19) means two things: its program is `wiregent-hw.exe`, and its
//! process is the one the service manager runs as the `wiregent-hw` service. The name alone isn't enough: any
//! program of the same user can be called `wiregent-hw.exe` and take the pipe name while the service restarts.

use crate::proto::{self, Request, Response};
use anyhow::{bail, Context, Result};
use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_MODE, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS};
use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

/// The service's program name, which clients insist on.
pub const SERVICE_IMAGE: &str = "wiregent-hw.exe";
/// The service's name in the service manager.
pub const SERVICE_NAME: &str = "wiregent-hw";

pub use crate::client_identity::Client;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A process's image path and user SID, read from the process itself.
pub fn process_identity(pid: u32) -> Result<Client> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).with_context(|| format!("opening process {pid}"))?;
        let close = |h: HANDLE| {
            let _ = CloseHandle(h);
        };
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let image = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).map(|_| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
        let mut tok = HANDLE::default();
        let sid = OpenProcessToken(h, TOKEN_QUERY, &mut tok).and_then(|_| {
            let mut need = 0u32;
            let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut need);
            let mut tb = vec![0u8; need as usize];
            let r = GetTokenInformation(tok, TokenUser, Some(tb.as_mut_ptr().cast()), need, &mut need).and_then(|_| {
                let user = &*(tb.as_ptr() as *const TOKEN_USER);
                let mut s = PWSTR::null();
                ConvertSidToStringSidW(user.User.Sid, &mut s)?;
                let text = s.to_string().unwrap_or_default();
                let _ = LocalFree(Some(HLOCAL(s.0.cast())));
                Ok(text)
            });
            close(tok);
            r
        });
        close(h);
        Ok(Client { pid, image: image.context("reading the process image")?, sid: sid.context("reading the process user")? })
    }
}

/// The SID of the user this process runs as.
pub fn current_sid() -> Result<String> {
    Ok(process_identity(std::process::id())?.sid)
}


/// How the client decides the pipe's server is the one it meant to reach.
pub enum ServerCheck<'a> {
    /// The server's program name only: tests, and a `wiregent-hw --console` run during development.
    Image(&'a str),
    /// Production: the program name, and the server's process id must be the one `pid` reports for the
    /// service ([`service_pid`] in production; tests pass their own).
    Service { image: &'a str, pid: &'a dyn Fn() -> Result<u32> },
}

impl ServerCheck<'_> {
    fn image(&self) -> &str {
        match self {
            ServerCheck::Image(i) | ServerCheck::Service { image: i, .. } => i,
        }
    }
}

/// The process id the service manager reports for the hardware service (a read-only query). Fails when the
/// service isn't installed or isn't running (a stopped service has no process).
pub fn service_pid() -> Result<u32> {
    let name = wide(SERVICE_NAME);
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).context("opening the service manager")?;
        let r = OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_QUERY_STATUS).context("the hardware service isn't installed").and_then(|svc| {
            let mut buf = vec![0u8; std::mem::size_of::<SERVICE_STATUS_PROCESS>()];
            let mut need = 0u32;
            let q = QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, Some(&mut buf), &mut need).context("asking the service manager about the hardware service");
            let _ = CloseServiceHandle(svc);
            q?;
            let st = std::ptr::read_unaligned(buf.as_ptr() as *const SERVICE_STATUS_PROCESS);
            if st.dwProcessId == 0 {
                bail!("the hardware service isn't running");
            }
            Ok(st.dwProcessId)
        });
        let _ = CloseServiceHandle(scm);
        r
    }
}

/// Debug builds only: `WIREGENT_HW_CONSOLE=1` lets the app talk to a `wiregent-hw --console` run, which has
/// the service's program name but isn't the service. Release builds always check the service's process.
fn console_override() -> bool {
    cfg!(debug_assertions) && std::env::var_os("WIREGENT_HW_CONSOLE").is_some_and(|v| v == "1")
}

/// Client side: connect (identification-only, so the server can never impersonate the caller), check the
/// server, say hello, send one request, return the answer. Asking for [`SERVICE_IMAGE`] means the real
/// service, so the server's process must also be the service's ([`ServerCheck::Service`] with
/// [`service_pid`]); any other program name is checked by name only.
pub fn request(name: &str, req: &Request, server_image: &str) -> Result<Response> {
    if server_image.eq_ignore_ascii_case(SERVICE_IMAGE) && !console_override() {
        request_checked(name, req, &ServerCheck::Service { image: SERVICE_IMAGE, pid: &service_pid })
    } else {
        request_checked(name, req, &ServerCheck::Image(server_image))
    }
}

/// [`request`] with the server check spelled out.
pub fn request_checked(name: &str, req: &Request, check: &ServerCheck) -> Result<Response> {
    let w = wide(name);
    let open = || unsafe { CreateFileW(PCWSTR(w.as_ptr()), GENERIC_READ.0 | GENERIC_WRITE.0, FILE_SHARE_MODE(0), None, OPEN_EXISTING, SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, None) };
    // Every pipe instance can be briefly busy (the server is still finishing the last client): wait for one,
    // as Windows documents for named-pipe clients, then retry. A few tries, at most ~2 s in all.
    let mut h = open();
    for _ in 0..4 {
        match &h {
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_PIPE_BUSY.to_hresult() => {
                let _ = unsafe { windows::Win32::System::Pipes::WaitNamedPipeW(PCWSTR(w.as_ptr()), 500) };
                h = open();
            }
            _ => break,
        }
    }
    let h = h.with_context(|| format!("connecting to {name} (is the Wiregent hardware service running?)"))?;
    let mut spid = 0u32;
    let server = unsafe { GetNamedPipeServerProcessId(h, &mut spid) }.map_err(anyhow::Error::from).and_then(|_| process_identity(spid));
    // From here the handle is owned by the File, which closes it.
    let mut f = unsafe { std::fs::File::from_raw_handle(h.0) };
    let server = server.context("identifying the pipe's server")?;
    let server_image = check.image();
    let image = server.image.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if image != server_image.to_ascii_lowercase() {
        bail!("{name} is served by {}, not {server_image}: refusing to talk to it", server.image.display());
    }
    if let ServerCheck::Service { pid, .. } = check {
        match pid() {
            Ok(expected) if expected == server.pid => {}
            Ok(expected) => bail!("{name} is served by process {} ({}), but the hardware service is process {expected}: another program holds the pipe, refusing to talk to it", server.pid, server.image.display()),
            Err(e) => bail!("{name} is served by process {} ({}), and the hardware service's own process can't be confirmed ({e:#}): refusing to talk to it", server.pid, server.image.display()),
        }
    }
    proto::write_frame(&mut f, &proto::encode(&Request::Hello { proto: proto::PROTO, build: proto::BUILD.into() })?)?;
    let hello: Response = serde_json::from_slice(&proto::read_frame(&mut f)?)?;
    match hello {
        Response::Hello { proto: p, .. } if p == proto::PROTO => {}
        Response::Hello { proto: p, build } => bail!("the hardware service speaks protocol {p} (build {build}); this program speaks {}: update the service", proto::PROTO),
        other => bail!("unexpected answer to hello: {other:?}"),
    }
    proto::write_frame(&mut f, &proto::encode(req)?)?;
    Ok(serde_json::from_slice(&proto::read_frame(&mut f)?)?)
}

/// Whether the hardware service's pipe exists (without connecting to it).
pub fn service_present() -> bool {
    let w = wide(proto::PIPE_NAME);
    unsafe { windows::Win32::System::Pipes::WaitNamedPipeW(PCWSTR(w.as_ptr()), 1) }.as_bool()
}
