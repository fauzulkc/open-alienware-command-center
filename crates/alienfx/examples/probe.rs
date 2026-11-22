//! Read-only probe of the AlienFX lighting interfaces (VID 187C and 0D62), printed as JSON.
//! Opens each matching HID interface with access 0 and reads attributes and caps; sends no reports.
//! Run: `cargo run -p alienfx --example probe`

#[cfg(windows)]
fn main() {
    use serde_json::json;

    let hex = |v: u16| format!("{v:04X}");
    match alienfx::probe::probe() {
        Ok(res) => {
            let interfaces: Vec<_> = res
                .interfaces
                .iter()
                .map(|i| {
                    json!({
                        "path": i.path,
                        "vid": hex(i.vid),
                        "pid": hex(i.pid),
                        "version": hex(i.version),
                        "usage_page": hex(i.usage_page),
                        "usage": hex(i.usage),
                        "input_len": i.input_len,
                        "output_len": i.output_len,
                        "feature_len": i.feature_len,
                        "api": i.api.map(|a| format!("v{}", a.number())),
                        "report_len": i.report_len,
                        "error": i.error,
                    })
                })
                .collect();
            let out = json!({
                "hid_interfaces_seen": res.hid_interfaces_seen,
                "probed_vids": ["187C", "0D62"],
                "interfaces": interfaces,
            });
            println!("{}", serde_json::to_string_pretty(&out).expect("json"));
        }
        Err(e) => {
            eprintln!("probe failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the AlienFX probe is Windows-only");
}
