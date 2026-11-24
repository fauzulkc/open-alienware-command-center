//! L0 write test (docs/lighting-and-macros-parity.md): one colour on one zone light (API v4) or one key
//! (API v5), the way alien-tools sends it, and nothing else. Live colours only: no controller-memory write
//! is ever sent (power-state slots, saved state), and every report passes the crate's allow-list first.
//!
//!   write_test zone <id> <r> <g> <b> [--send]     e.g. zone 1 255 0 0   (1 = the alien head)
//!   write_test key  <id> <r> <g> <b> [--send]     e.g. key 0 0 255 0
//!   write_test status                             the zone controller's status byte (a read)
//!
//! Without `--send` it only prints the reports. Run it with AWCC's lighting programs paused (one writer).

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(match win::run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    });
}

#[cfg(windows)]
mod win {
    use alienfx::probe::{probe, HidInterface};
    use alienfx::{allowed, is_memory_write, v4, v5, Api, ApiVersion, Rgb, Step};
    use windows::core::PCWSTR;
    use windows::Win32::Devices::HumanInterfaceDevice::{HidD_GetFeature, HidD_GetInputReport, HidD_SetFeature, HidD_SetOutputReport};
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

    struct Dev(HANDLE);
    impl Drop for Dev {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn find(api: ApiVersion) -> Result<HidInterface, String> {
        probe()?.interfaces.into_iter().find(|i| i.api == Some(api)).ok_or_else(|| format!("no {api:?} lighting interface found"))
    }

    fn open(path: &str) -> Result<Dev, String> {
        let w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let h = unsafe { CreateFileW(PCWSTR(w.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }.map_err(|e| format!("open {path}: {e}"))?;
        Ok(Dev(h))
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
    }

    fn check(bytes: &[u8], api: Api) -> Result<(), String> {
        if !allowed(bytes, api) {
            return Err(format!("refused by the allow-list: {}", hex(bytes)));
        }
        if is_memory_write(bytes, api) {
            return Err(format!("refused: a controller-memory write: {}", hex(bytes)));
        }
        Ok(())
    }

    fn v4_status(d: &Dev) -> Result<u8, String> {
        let mut buf = [0u8; v4::REPORT_LEN];
        unsafe { HidD_GetInputReport(d.0, buf.as_mut_ptr().cast(), buf.len() as u32) }.then_some(()).ok_or_else(|| format!("read status: {}", std::io::Error::last_os_error()))?;
        v4::status_byte(&buf).ok_or_else(|| "no status byte".into())
    }

    fn wait_ready(d: &Dev) -> Result<(), String> {
        for _ in 0..100 {
            let s = v4_status(d)?;
            if matches!(v4::readiness(s), v4::Readiness::Ready) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(v4::POLL_INTERVAL_MS as u64));
        }
        Err("the zone controller never reported ready (2 s); nothing was sent".into())
    }

    pub fn run(args: Vec<String>) -> Result<(), String> {
        let send = args.iter().any(|a| a == "--send");
        let a: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--send").collect();
        let num = |i: usize| -> Result<u8, String> { a.get(i).ok_or("missing argument")?.parse::<u8>().map_err(|e| format!("{}: {e}", a[i])) };
        match a.first().copied() {
            Some("status") => {
                let i = find(ApiVersion::V4)?;
                let d = open(&i.path)?;
                let s = v4_status(&d)?;
                println!("zone controller status: {s} ({:?})", v4::readiness(s));
                Ok(())
            }
            Some("zone") => {
                let (id, c) = (num(1)?, Rgb::new(num(2)?, num(3)?, num(4)?));
                let mut steps = v4::reset();
                steps.push(Step::Write(v4::set_one_color(id, c)));
                steps.push(Step::Write(v4::update()));
                for s in &steps {
                    if let Some(r) = s.report() {
                        check(r.as_bytes(), Api::V4)?;
                    }
                }
                let i = find(ApiVersion::V4)?;
                println!("zone controller {:04X}:{:04X} {}", i.vid, i.pid, i.path);
                let d = if send { Some(open(&i.path)?) } else { None };
                for s in steps {
                    match (&s, &d) {
                        (Step::WaitReady, Some(d)) => wait_ready(d)?,
                        (Step::Write(r), Some(d)) => {
                            let mut b = r.into_bytes_copy();
                            unsafe { HidD_SetOutputReport(d.0, b.as_mut_ptr().cast(), b.len() as u32) }.then_some(()).ok_or_else(|| format!("send {}: {}", hex(&b), std::io::Error::last_os_error()))?;
                            println!("sent  {}", hex(&b));
                        }
                        (Step::Write(r), None) => println!("would send {}", hex(r.as_bytes())),
                        (other, _) => println!("step {other:?}"),
                    }
                }
                if let Some(d) = &d {
                    println!("status after: {}", v4_status(d)?);
                }
                Ok(())
            }
            Some(cmd @ ("key" | "only" | "preset")) => {
                let keys: Vec<v5::Key> = if cmd == "preset" {
                    // `preset <file>`: the keyboard groups of `cockpit --json lighting show` (AWCC's active
                    // preset, read from a copy of its database): every LED id in a group gets the group's colour.
                    let text = std::fs::read_to_string(a.get(1).ok_or("missing file")?).map_err(|e| e.to_string())?;
                    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
                    let kb = v["devices"].as_array().and_then(|d| d.iter().find(|x| x["device"].as_str().is_some_and(|n| n.contains("Mechanical")))).ok_or("no keyboard in the preset")?;
                    let mut keys = Vec::new();
                    for g in kb["groups"].as_array().ok_or("no groups")? {
                        let hexc = g["colors"][0].as_str().ok_or("group without a colour")?.trim_start_matches('#');
                        let p = |i: usize| u8::from_str_radix(&hexc[i..i + 2], 16).map_err(|e| e.to_string());
                        let c = Rgb::new(p(0)?, p(2)?, p(4)?);
                        for id in g["leds"].as_array().ok_or("group without leds")? {
                            keys.push(v5::Key::new(id.as_u64().ok_or("bad led id")? as u8, c));
                        }
                    }
                    println!("{} keys from the preset", keys.len());
                    keys
                } else {
                    let (id, c) = (num(1)?, Rgb::new(num(2)?, num(3)?, num(4)?));
                    // `only`: every key of this keyboard (AWCC's 91 LED ids) dark, then this one key lit.
                    if cmd == "only" {
                        X17_KEYS.iter().map(|&k| v5::Key::new(k, if k == id { c } else { Rgb::BLACK })).collect()
                    } else {
                        vec![v5::Key::new(id, c)]
                    }
                };
                let steps = v5::apply_keys(&keys).map_err(|e| e.to_string())?;
                for s in &steps {
                    if let Some(r) = s.report() {
                        check(r.as_bytes(), Api::V5)?;
                    }
                }
                let i = find(ApiVersion::V5)?;
                println!("keyboard {:04X}:{:04X} {}", i.vid, i.pid, i.path);
                let d = if send { Some(open(&i.path)?) } else { None };
                for s in steps {
                    match (&s, &d) {
                        (Step::Write(r), Some(d)) => {
                            let mut b = r.into_bytes_copy();
                            unsafe { HidD_SetFeature(d.0, b.as_mut_ptr().cast(), b.len() as u32) }.then_some(()).ok_or_else(|| format!("send {}: {}", hex(&b), std::io::Error::last_os_error()))?;
                            println!("sent  {}", hex(&b[..12]));
                        }
                        (Step::ReadStatus, Some(d)) => {
                            let mut b = [0u8; v5::REPORT_LEN];
                            b[0] = v5::REPORT_ID;
                            unsafe { HidD_GetFeature(d.0, b.as_mut_ptr().cast(), b.len() as u32) }.then_some(()).ok_or_else(|| format!("read status: {}", std::io::Error::last_os_error()))?;
                            println!("status {:?}", v5::status_byte(&b));
                        }
                        (Step::Write(r), None) => println!("would send {}", hex(&r.as_bytes()[..12])),
                        (other, _) => println!("step {other:?}"),
                    }
                }
                Ok(())
            }
            _ => Err("usage: write_test zone|key <id> <r> <g> <b> [--send] | status".into()),
        }
    }

    /// The x17 R2 Cherry keyboard's LED ids, from AWCC's own description of it (FXMetadata, 91 LEDs).
    const X17_KEYS: [u8; 91] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 35, 40, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 55, 59, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 94, 100, 101, 103, 104, 105, 109, 111, 112, 113, 114, 115, 133, 134, 135,
    ];

    trait CopyBytes {
        fn into_bytes_copy(&self) -> Vec<u8>;
    }
    impl<const N: usize> CopyBytes for alienfx::Report<N> {
        fn into_bytes_copy(&self) -> Vec<u8> {
            self.as_bytes().to_vec()
        }
    }
}
