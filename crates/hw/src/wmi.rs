//! A minimal WMI client: connect, query, and call `AWCCWmiMethodFunction`: reads, and thermal-mode writes
//! through `write` (allow-listed; gated in `crate::control`).
//!
//! Every call has a timeout (review 2026-11-09, finding 7): a stuck ACPI/EC method must not freeze the
//! reader, and a WMI restart (`RPC_E_DISCONNECTED`) is answered by reconnecting, not by failing forever.

use anyhow::{anyhow, bail, Context, Result};
use std::cell::RefCell;
use windows::core::{BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::{IEnumWbemClassObject, IWbemCallResult, IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE};
use wiregent_oem_alienware::{allowed, Call, ERROR};

/// How long one firmware call may take before it counts as failed.
pub const CALL_TIMEOUT_MS: i32 = 3_000;
/// How long a query may wait for its next row.
pub const QUERY_TIMEOUT_MS: i32 = 10_000;
const WBEM_S_TIMEDOUT: i32 = 0x0004_0004;
const RPC_E_DISCONNECTED: i32 = 0x8001_0108_u32 as i32;
const RPC_S_SERVER_UNAVAILABLE: i32 = 0x8007_06BA_u32 as i32;

pub struct Wmi {
    svc: IWbemServices,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

impl Wmi {
    pub fn connect(namespace: &str) -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            // RPC_E_TOO_LATE when something already set it: fine.
            let _ = CoInitializeSecurity(None, -1, None, None, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE, None);
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).context("creating the WMI locator")?;
            let svc = locator.ConnectServer(&BSTR::from(namespace), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None).with_context(|| format!("connecting to {namespace}"))?;
            CoSetProxyBlanket(&svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE).context("setting the WMI proxy security")?;
            Ok(Wmi { svc })
        }
    }

    pub fn rows(&self, wql: &str) -> Result<Vec<IWbemClassObject>> {
        unsafe {
            let e: IEnumWbemClassObject = self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(wql), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None).with_context(|| format!("running {wql}"))?;
            let mut out = Vec::new();
            loop {
                let mut row = [None];
                let mut n = 0u32;
                let hr = e.Next(QUERY_TIMEOUT_MS, &mut row, &mut n);
                if hr.0 == WBEM_S_TIMEDOUT {
                    bail!("{wql} timed out after {} s", QUERY_TIMEOUT_MS / 1000);
                }
                if hr.is_err() {
                    let msg = windows::core::Error::from(hr).message();
                    let why = if hr.0 as u32 == 0x8004_1003 { "access denied: Windows needs admin to read this".to_string() } else if msg.trim().is_empty() { format!("WMI error 0x{:08X}", hr.0 as u32) } else { msg.to_string() };
                    bail!("{why}");
                }
                if n == 0 {
                    break;
                }
                if let Some(r) = row[0].take() {
                    out.push(r);
                }
            }
            Ok(out)
        }
    }

    /// Text values of `props` for every row of `wql` (empty text when a value is missing).
    pub fn query(&self, wql: &str, props: &[&str]) -> Result<Vec<Vec<String>>> {
        let rows = self.rows(wql)?;
        Ok(rows.iter().map(|r| props.iter().map(|p| get_text(r, p)).collect()).collect())
    }
}

fn get(obj: &IWbemClassObject, name: &str) -> Option<VARIANT> {
    let w = wide(name);
    let mut v = VARIANT::default();
    unsafe { obj.Get(PCWSTR(w.as_ptr()), 0, &mut v, None, None) }.ok()?;
    (!v.is_empty()).then_some(v)
}

pub fn get_text(obj: &IWbemClassObject, name: &str) -> String {
    let Some(v) = get(obj, name) else { return String::new() };
    if let Ok(b) = BSTR::try_from(&v) {
        return b.to_string();
    }
    if let Ok(n) = i64::try_from(&v) {
        return n.to_string();
    }
    if let Ok(n) = i32::try_from(&v) {
        return n.to_string();
    }
    if let Ok(n) = u32::try_from(&v) {
        return n.to_string();
    }
    format!("{v}")
}

struct Session {
    wmi: Wmi,
    path: BSTR,
    instance: String,
    sigs: Vec<(&'static str, IWbemClassObject)>,
}

fn open_session() -> Result<Session> {
    let wmi = Wmi::connect("ROOT\\WMI")?;
    let rows = wmi.rows("SELECT * FROM AWCCWmiMethodFunction").context("opening AWCCWmiMethodFunction (this needs admin rights)")?;
    let inst = rows.first().ok_or_else(|| anyhow!("no AWCCWmiMethodFunction instance: not an Alienware with AWCC firmware, or not elevated"))?;
    let path = BSTR::from(get_text(inst, "__PATH"));
    let instance = get_text(inst, "InstanceName");
    let mut class = None;
    unsafe { wmi.svc.GetObject(&BSTR::from("AWCCWmiMethodFunction"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class), None) }.context("reading the AWCC class")?;
    let class: IWbemClassObject = class.ok_or_else(|| anyhow!("no AWCC class object"))?;
    let mut sigs = Vec::new();
    // Signatures for the read methods and Thermal_Control (thermal-mode writes, through `write` only).
    for name in ["Thermal_Information", "GetFanSensors", "Thermal_Control"] {
        let w = wide(name);
        let mut input = None;
        unsafe { class.GetMethod(PCWSTR(w.as_ptr()), 0, &mut input, std::ptr::null_mut()) }.with_context(|| format!("reading {name}'s signature"))?;
        sigs.push((name, input.ok_or_else(|| anyhow!("{name} has no input signature"))?));
    }
    Ok(Session { wmi, path, instance, sigs })
}

/// The AWCC firmware object, opened for reads. Reconnects once when WMI itself went away.
pub struct Awcc {
    session: RefCell<Session>,
}

impl Awcc {
    pub fn open() -> Result<Self> {
        Ok(Awcc { session: RefCell::new(open_session()?) })
    }

    pub fn instance(&self) -> String {
        self.session.borrow().instance.clone()
    }

    fn call(s: &Session, c: Call) -> windows::core::Result<u32> {
        let name = c.method.wmi_name();
        let sig = &s.sigs.iter().find(|(n, _)| *n == name).expect("read signature").1;
        unsafe {
            let input = sig.SpawnInstance(0)?;
            let arg = wide("arg2");
            input.Put(PCWSTR(arg.as_ptr()), 0, &VARIANT::from(c.packed() as i32), 0)?;
            // Semi-synchronous: the call returns at once and the answer is awaited with a timeout.
            let mut pending: Option<IWbemCallResult> = None;
            s.wmi.svc.ExecMethod(&s.path, &BSTR::from(name), WBEM_FLAG_RETURN_IMMEDIATELY, None, &input, None, Some(&mut pending))?;
            let pending = pending.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(WBEM_S_TIMEDOUT)))?;
            let out = pending.GetResultObject(CALL_TIMEOUT_MS)?;
            let v = get(&out, "argr").ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(ERROR as i32)))?;
            u32::try_from(&v).or_else(|_| i32::try_from(&v).map(|n| n as u32))
        }
    }

    /// One firmware write: only `Thermal_Control` calls on the allow-list, which only `Discovery::write` can
    /// build (a reported profile or fan id). Callers go through `crate::control`, which adds the gates.
    pub fn write(&self, c: Call) -> Result<u32> {
        let name = c.method.wmi_name();
        if !c.is_write() || !allowed(name, c.op) {
            bail!("refused: {name} op 0x{:02X} is not a write on the allow-list", c.op);
        }
        let n = Self::call(&self.session.borrow(), c).map_err(|e| anyhow!("{name} op 0x{:02X}: {}", c.op, e.message()))?;
        if n == ERROR {
            bail!("{name} op 0x{:02X}: the firmware refused the change", c.op);
        }
        Ok(n)
    }

    /// One read call. Writes, and anything off the allow-list, are refused before WMI sees them; the
    /// firmware's 0xFFFFFFFF ("unsupported") is an error, never a value.
    pub fn read(&self, c: Call) -> Result<u32> {
        let name = c.method.wmi_name();
        if c.is_write() || !allowed(name, c.op) {
            bail!("refused: {name} op 0x{:02X} is not a read on the allow-list", c.op);
        }
        let first = Self::call(&self.session.borrow(), c);
        let n = match first {
            Ok(n) => n,
            Err(e) if [RPC_E_DISCONNECTED, RPC_S_SERVER_UNAVAILABLE].contains(&e.code().0) => {
                *self.session.borrow_mut() = open_session().context("reconnecting to WMI")?;
                Self::call(&self.session.borrow(), c).with_context(|| format!("{name} after reconnecting"))?
            }
            Err(e) => return Err(anyhow!("{name} op 0x{:02X}: {}", c.op, e.message())),
        };
        if n == ERROR {
            bail!("{name} op 0x{:02X} answered 'unsupported'", c.op);
        }
        Ok(n)
    }
}
