//! The pipe protocol (design §4): length-prefixed JSON, at most 16 KB a frame, a versioned handshake, and a
//! closed set of commands. H1b's commands are read-only.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};

pub const PROTO: u32 = 1;
pub const MAX_FRAME: usize = 16 * 1024;
pub const PIPE_NAME: &str = r"\\.\pipe\wiregent-hw";
pub const BUILD: &str = env!("CARGO_PKG_VERSION");

/// What a client may ask. Anything else fails to parse and gets an `Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Hello { proto: u32, build: String },
    Snapshot,
    Health,
    Lists,
    /// Switch the thermal mode ("0xA0"). Checked again in the service: machine gate, reported ids, rate limit.
    SetProfile { profile: String },
    /// Raise (or reset to 0) one fan's boost, 0..=100 ("0x32"). Checked again in the service.
    SetFanBoost { fan: String, level: u8 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Response {
    Hello { proto: u32, build: String },
    /// The latest reading; `None` until the first tick, `stale_ms` = how old it is.
    Snapshot { snapshot: Option<Value>, stale_ms: Option<u64> },
    Health { health: Value },
    Lists { lists: Value },
    ModeChanged { change: Value },
    BoostChanged { change: Value },
    Error { message: String },
}

/// One frame: 4-byte little-endian length, then that many bytes of JSON.
pub fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(v)?;
    if body.is_empty() || body.len() > MAX_FRAME {
        bail!("a frame must be 1..={MAX_FRAME} bytes, this one is {}", body.len());
    }
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend(body);
    Ok(out)
}

/// The body length a header announces; refuses empty and oversized frames before reading them.
pub fn frame_len(header: [u8; 4]) -> Result<usize> {
    let n = u32::from_le_bytes(header) as usize;
    if n == 0 || n > MAX_FRAME {
        bail!("refused a frame of {n} bytes (limit {MAX_FRAME})");
    }
    Ok(n)
}

pub fn read_frame(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h).context("reading a frame header")?;
    let mut body = vec![0u8; frame_len(h)?];
    r.read_exact(&mut body).context("reading a frame body")?;
    Ok(body)
}

pub fn write_frame(w: &mut impl Write, frame: &[u8]) -> Result<()> {
    w.write_all(frame)?;
    w.flush()?;
    Ok(())
}

/// A request body, or the `Error` a client gets back for it.
pub fn parse_request(body: &[u8]) -> std::result::Result<Request, Response> {
    let refuse = |why: String| Response::Error { message: format!("not a request this service accepts: {why}") };
    let v: Value = serde_json::from_slice(body).map_err(|e| refuse(e.to_string()))?;
    let obj = v.as_object().ok_or_else(|| refuse("not an object".into()))?;
    // Exact field sets: serde's deny_unknown_fields doesn't catch extras on tag-only commands.
    let allowed: &[&str] = match obj.get("cmd").and_then(|c| c.as_str()) {
        Some("hello") => &["cmd", "proto", "build"],
        Some("snapshot" | "health" | "lists") => &["cmd"],
        Some("set-profile") => &["cmd", "profile"],
        Some("set-fan-boost") => &["cmd", "fan", "level"],
        Some(other) => return Err(refuse(format!("unknown command '{other}'"))),
        None => return Err(refuse("no cmd".into())),
    };
    if let Some(extra) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(refuse(format!("unexpected field '{extra}'")));
    }
    serde_json::from_value(v).map_err(|e| refuse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        for req in [Request::Hello { proto: PROTO, build: "x".into() }, Request::Snapshot, Request::Health, Request::Lists, Request::SetProfile { profile: "0xA0".into() }, Request::SetFanBoost { fan: "0x32".into(), level: 50 }] {
            let f = encode(&req).unwrap();
            let body = read_frame(&mut f.as_slice()).unwrap();
            assert_eq!(parse_request(&body).unwrap(), req);
        }
        assert_eq!(serde_json::to_string(&Request::Snapshot).unwrap(), r#"{"cmd":"snapshot"}"#);
    }

    /// C-16: oversized, empty, truncated and unknown frames are refused without reading past the limit.
    #[test]
    fn bad_frames_are_refused() {
        assert!(frame_len(((MAX_FRAME + 1) as u32).to_le_bytes()).is_err());
        assert!(frame_len(0u32.to_le_bytes()).is_err());
        assert!(frame_len(u32::MAX.to_le_bytes()).is_err());
        let mut truncated = encode(&Request::Snapshot).unwrap();
        truncated.truncate(6);
        assert!(read_frame(&mut truncated.as_slice()).is_err());
        for body in [&br#"{"cmd":"set-fan-boost","fan":"0x32","level":256}"#[..], br#"{"cmd":"set-fan-boost","fan":"0x32","level":50,"x":1}"#, br#"{"cmd":"set-profile","profile":"0xA0","fan":50}"#, br#"{"cmd":"snapshot","extra":1}"#, br#"not json"#, &[0xFF, 0xFE]] {
            assert!(matches!(parse_request(body), Err(Response::Error { .. })), "{:?}", String::from_utf8_lossy(body));
        }
        let big = Response::Error { message: "x".repeat(MAX_FRAME) };
        assert!(encode(&big).is_err(), "the service never sends an oversized frame either");
    }
}
