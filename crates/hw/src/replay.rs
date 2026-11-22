//! A firmware that answers from a capture (tests, and the app's demo mode). It answers reads like the real
//! one; writes are recorded and a profile write changes what the current-profile read returns, so the
//! mode-switch path can be tested end to end without hardware.

use crate::control::FirmwareWrite;
use crate::Firmware;
use anyhow::{anyhow, bail};
use std::sync::{Arc, Mutex};
use wiregent_oem_alienware::{parse_capture, Call, Method, OP_ACTIVATE_PROFILE, OP_CURRENT_PROFILE, OP_FAN_BOOST, OP_SET_FAN_BOOST, REFERENCE_CAPTURE};

#[derive(Clone, Default)]
struct Live {
    failing: Vec<u8>,
    profile: Option<u32>,
    boost: std::collections::BTreeMap<u8, u32>,
    writes: Vec<u32>,
    ignore_writes: bool,
}

#[derive(Clone)]
pub struct Replay {
    rows: Arc<Vec<(String, u32, u32)>>,
    live: Arc<Mutex<Live>>,
}

impl Replay {
    pub fn from_capture(text: &str) -> Self {
        Replay { rows: Arc::new(parse_capture(text)), live: Arc::new(Mutex::new(Live::default())) }
    }

    /// The x17 R2 capture of 2026-10-07 (G-Mode, CPU 100 °C, fan 0x3B at 0 RPM).
    pub fn october() -> Self {
        Self::from_capture(REFERENCE_CAPTURE)
    }

    /// From now on, every read with this op fails.
    pub fn fail_op(&self, op: u8) {
        self.live.lock().unwrap().failing.push(op);
    }

    /// Accept writes but don't act on them (a firmware that ignores the change).
    pub fn ignore_writes(&self) {
        self.live.lock().unwrap().ignore_writes = true;
    }

    /// Packed values of every write so far.
    pub fn writes(&self) -> Vec<u32> {
        self.live.lock().unwrap().writes.clone()
    }
}

impl Firmware for Replay {
    fn read(&self, c: Call) -> anyhow::Result<u32> {
        if c.is_write() {
            bail!("refused: reads only through read()");
        }
        let live = self.live.lock().unwrap();
        if live.failing.contains(&c.op) {
            bail!("op 0x{:02X} failing (test)", c.op);
        }
        if c.method == Method::ThermalInformation && c.op == OP_CURRENT_PROFILE {
            if let Some(p) = live.profile {
                return Ok(p);
            }
        }
        if c.method == Method::ThermalInformation && c.op == OP_FAN_BOOST {
            if let Some(b) = live.boost.get(&c.arg1) {
                return Ok(*b);
            }
        }
        self.rows.iter().find(|(m, p, _)| m == c.method.wmi_name() && *p == c.packed()).map(|r| r.2).ok_or_else(|| anyhow!("no answer in the capture for {:?}", c))
    }
}

impl FirmwareWrite for Replay {
    fn write(&self, c: Call) -> anyhow::Result<u32> {
        if !c.is_write() {
            bail!("refused: not a write");
        }
        let mut live = self.live.lock().unwrap();
        live.writes.push(c.packed());
        if c.op == OP_ACTIVATE_PROFILE && !live.ignore_writes {
            live.profile = Some(c.arg1 as u32);
        }
        if c.op == OP_SET_FAN_BOOST && !live.ignore_writes {
            live.boost.insert(c.arg1, c.arg2 as u32);
        }
        Ok(0)
    }
}
