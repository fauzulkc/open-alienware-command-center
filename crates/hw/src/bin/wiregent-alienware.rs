//! `wiregent-alienware`: the `wiregent.alienware` plugin program (plugins/alienware/plugin.json).
//!
//! As a plugin it speaks the module contract (docs/MODULES.md): one request line on stdin
//! (`{"id","method","params"}`), JSON event lines out, ending in `result` or `error`.
//! By hand: `wiregent-alienware readings | health [--days 30] | drift | watch [--secs 60]`.
//!
//! H1 is read-only. The firmware readings need admin until the hardware service exists (H1b); without it,
//! `readings` still returns CPU power and says why the firmware part is missing.

use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() && !std::io::stdin().is_terminal() {
        contract();
    } else {
        cli(&args);
    }
}

fn contract() {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    let req: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let id = req.get("id").and_then(|v| v.as_str()).unwrap_or("1").to_string();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let out = match run(method, &params) {
        Ok(data) => json!({ "id": id, "event": "result", "data": data }),
        Err(e) => json!({ "id": id, "event": "error", "data": { "message": format!("{e:#}") } }),
    };
    println!("{out}");
}

fn cli(args: &[String]) {
    let method = args.first().map(String::as_str).unwrap_or("help");
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u64>().ok());
    let params = json!({ "days": flag("--days").unwrap_or(30), "secs": flag("--secs").unwrap_or(60) });
    match run(method, &params) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run(method: &str, params: &Value) -> anyhow::Result<Value> {
    match method {
        "readings" => readings(),
        "health" => health(params.get("days").and_then(|v| v.as_u64()).unwrap_or(30) as u32),
        "drift" => drift(),
        "watch" => watch(params.get("secs").and_then(|v| v.as_u64()).unwrap_or(60)),
        "set_profile" | "set_fan_boost" | "restore" => anyhow::bail!("'{method}' changes the hardware; it arrives with H2, after the write tests on this laptop"),
        "help" | "" => Ok(json!({ "methods": ["readings", "health", "drift", "watch"], "usage": "wiregent-alienware readings | health [--days 30] | drift | watch [--secs 60]" })),
        other => anyhow::bail!("no such action '{other}'"),
    }
}

#[cfg(windows)]
fn power() -> Value {
    match wiregent_hw::power::sample(std::time::Duration::from_millis(500)) {
        Ok(rows) => {
            let pick = |inst: &str| rows.iter().find(|(p, i, _)| p.contains("Energy Meter") && i == inst).and_then(|(_, _, v)| v.as_ref().ok().map(|mw| (mw / 100.0).round() / 10.0));
            let perf = rows.iter().find(|(p, _, _)| p.contains("% Processor Performance")).and_then(|(_, _, v)| v.as_ref().ok().copied());
            json!({ "cpuPackageW": pick("RAPL_Package0_PKG"), "cpuCoresW": pick("RAPL_Package0_PP0"), "cpuPerformancePct": perf })
        }
        Err(e) => json!({ "error": format!("{e:#}") }),
    }
}

#[cfg(windows)]
fn readings() -> anyhow::Result<Value> {
    use wiregent_hw::reader::Reader;
    let power = power();
    // The hardware service, when installed, is the normal source: it holds the only firmware connection.
    if wiregent_hw::client::service_present() {
        use wiregent_hw::proto::{Request, Response, PIPE_NAME};
        return match wiregent_hw::client::request(PIPE_NAME, &Request::Snapshot, wiregent_hw::client::SERVICE_IMAGE)? {
            Response::Snapshot { snapshot, stale_ms } => Ok(json!({ "firmware": snapshot, "staleMs": stale_ms, "source": "service", "power": power })),
            Response::Error { message } => anyhow::bail!("the hardware service refused: {message}"),
            other => anyhow::bail!("unexpected answer from the hardware service: {other:?}"),
        };
    }
    if !wiregent_hw::is_admin() {
        return Ok(json!({ "firmware": null, "firmwareUnavailable": "Fan speeds, temperatures and the thermal mode come from the Alienware firmware, which Windows only lets an admin process read. They appear here once the Wiregent hardware service is installed (H1b), or when this runs from an admin prompt.", "power": power }));
    }
    let mut r = Reader::new(wiregent_hw::wmi::Awcc::open()?)?;
    let snap = r.tick(0);
    Ok(json!({ "firmware": snap, "lists": r.lists(), "power": power }))
}

#[cfg(windows)]
fn watch(secs: u64) -> anyhow::Result<Value> {
    use wiregent_hw::reader::Reader;
    anyhow::ensure!(wiregent_hw::is_admin(), "watch reads the firmware: run it from an admin prompt");
    let mut r = Reader::new(wiregent_hw::wmi::Awcc::open()?)?;
    let start = std::time::Instant::now();
    let (mut alerts, mut ticks, mut last) = (Vec::new(), 0u32, None);
    while start.elapsed().as_secs() < secs {
        let t = start.elapsed().as_millis() as u64;
        let s = r.tick(t);
        for a in &s.alerts {
            eprintln!("{:>5.0} s  {}", t as f64 / 1000.0, a.text());
            alerts.push(json!({ "atS": t / 1000, "alert": a, "text": a.text() }));
        }
        let hottest = s.sensors.iter().filter_map(|x| x.celsius).max();
        ticks += 1;
        last = Some(s);
        std::thread::sleep(std::time::Duration::from_millis(wiregent_oem_alienware::rules::interval_ms(hottest, true)));
    }
    Ok(json!({ "ticks": ticks, "alerts": alerts, "last": last }))
}

#[cfg(windows)]
fn health(days: u32) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(wiregent_hw::health::read(days))?)
}

#[cfg(windows)]
fn drift() -> anyhow::Result<Value> {
    use wiregent_hw::drift::{check, live, Rules};
    let d = check(&Rules::builtin(), &live());
    Ok(json!({ "ok": d.iter().all(|x| x.severity == "info"), "drift": d }))
}

#[cfg(not(windows))]
fn readings() -> anyhow::Result<Value> {
    anyhow::bail!("Windows only")
}
#[cfg(not(windows))]
fn watch(_: u64) -> anyhow::Result<Value> {
    anyhow::bail!("Windows only")
}
#[cfg(not(windows))]
fn health(_: u32) -> anyhow::Result<Value> {
    anyhow::bail!("Windows only")
}
#[cfg(not(windows))]
fn drift() -> anyhow::Result<Value> {
    anyhow::bail!("Windows only")
}
