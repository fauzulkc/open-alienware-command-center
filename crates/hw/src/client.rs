//! The hardware service's pipe, client side (design §4): connect identification-only (the service can never
//! impersonate the caller), check that the server is the hardware service, say hello, ask one thing.

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
use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

/// The service's program name, which clients insist on.
pub const SERVICE_IMAGE: &str = "wiregent-hw.exe";

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


/// Client side: connect (identification-only, so the server can never impersonate the caller), check the
/// server's process image, say hello, send one request, return the answer.
pub fn request(name: &str, req: &Request, server_image: &str) -> Result<Response> {
    let w = wide(name);
    let h = unsafe { CreateFileW(PCWSTR(w.as_ptr()), GENERIC_READ.0 | GENERIC_WRITE.0, FILE_SHARE_MODE(0), None, OPEN_EXISTING, SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, None) }.with_context(|| format!("connecting to {name} (is the Wiregent hardware service running?)"))?;
    let mut spid = 0u32;
    let server = unsafe { GetNamedPipeServerProcessId(h, &mut spid) }.map_err(anyhow::Error::from).and_then(|_| process_identity(spid));
    // From here the handle is owned by the File, which closes it.
    let mut f = unsafe { std::fs::File::from_raw_handle(h.0) };
    let server = server.context("identifying the pipe's server")?;
    let image = server.image.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if image != server_image.to_ascii_lowercase() {
        bail!("{name} is served by {}, not {server_image}: refusing to talk to it", server.image.display());
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
