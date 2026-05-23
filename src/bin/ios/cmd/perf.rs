/// Live performance monitoring via com.apple.instruments.dtservicehub / sysmontap.
///
/// Mirrors the pymobiledevice3 approach: query sysmonProcessAttributes and
/// sysmonSystemAttributes from deviceinfo on first connect so the config uses
/// exactly the attribute names the device reports, then subscribe to sysmontap.
use anyhow::Result;
use ios_rs::dtx::{self, AuxValue, DtxConn};
use ios_rs::tunnel::ConnectionMode;
use plist::{Dictionary, Value};
use std::io::Write;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cmd::open_session;
use crate::cmd::output::OutputMode;

// ── fallback attribute lists (used when deviceinfo query fails) ───────────────

const FALLBACK_PROC_ATTRS: &[&str] = &[
    "memVirtualSize", "cpuUsage", "ctxSwitch", "intWakeups",
    "physFootprint", "memResidentSize", "memAnon", "pid", "ppid", "name",
];

const FALLBACK_SYS_ATTRS: &[&str] = &[
    "physMemSize", "vmUsedCount", "vmFreeCount", "__vmSwapUsage",
    "diskBytesRead", "diskBytesWritten", "netBytesIn", "netBytesOut", "threadCount",
];

// ── data model ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProcSnap {
    pid:  u32,
    name: String,
    cpu:  f64,
    phys: u64,
    virt: u64,
}

#[derive(Debug, Clone)]
struct Snapshot {
    ts:        u64,
    cpu_total: f64,
    phys_mem:  u64,
    mem_used:  u64,
    processes: Vec<ProcSnap>,
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run(udid: Option<&str>, output: OutputMode, interval_ms: u64) -> Result<()> {
    let json_mode = output.is_json();
    let mut session = open_session(udid, ConnectionMode::Rsd)?;

    if !json_mode {
        print!("\x1b[?25l");
        std::io::stdout().flush().ok();
    }
    let _cursor = if json_mode { None } else { Some(CursorGuard) };
    let (cols, rows) = term_size();

    let sys_attrs: Vec<&str> = FALLBACK_SYS_ATTRS.to_vec();
    let proc_attrs: Vec<&str> = FALLBACK_PROC_ATTRS.to_vec();
    let config = build_config(interval_ms, &proc_attrs, &sys_attrs);

    let mut consecutive_failures: u32 = 0;

    loop {
        if consecutive_failures > 0 {
            let delay = Duration::from_millis(500 * (1u64 << consecutive_failures.min(4)));
            thread::sleep(delay);
        }

        match sample_once(&mut session, &config) {
            Ok(snap) => {
                consecutive_failures = 0;
                if json_mode { print_json(&snap); }
                else         { print_htop(&snap, cols, rows); }
            }
            Err(e) => {
                consecutive_failures += 1;
                if !json_mode { print_status(&format!("reconnecting… ({e})"), cols); }
                eprintln!("[perf] {e}");
            }
        }
    }
}

/// Open one DTX connection, subscribe to sysmontap, return the first snapshot.
fn sample_once(
    session: &mut ios_rs::tunnel::DeviceSession,
    config:  &[u8],
) -> Result<Snapshot> {
    let conn = connect_hub(session)
        .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
    conn.handshake().map_err(|e| anyhow::anyhow!("handshake: {e}"))?;

    // Open deviceinfo with fire-and-forget attr queries (server prerequisite on some iOS versions).
    // Do NOT use call() here — it causes dtservicehub on iOS 26 to close the connection.
    if let Ok(di) = conn.request_channel(
            "com.apple.instruments.server.services.deviceinfo") {
        let _ = conn.call_async(di, "sysmonProcessAttributes", &[]);
        let _ = conn.call_async(di, "sysmonSystemAttributes",  &[]);
    }

    let tap = conn.request_channel("com.apple.instruments.server.services.sysmontap")
        .map_err(|e| anyhow::anyhow!("sysmontap channel: {e}"))?;

    let rx_pos = conn.register_channel(tap);
    let rx_neg = conn.register_channel(-tap);
    let (data_tx, data_rx) = std::sync::mpsc::sync_channel::<dtx::DtxMessage>(64);
    for rx in [rx_pos, rx_neg] {
        let tx = data_tx.clone();
        thread::spawn(move || { while let Ok(m) = rx.recv() { if tx.send(m).is_err() { break; } } });
    }
    drop(data_tx);

    conn.call_async(tap, "setConfig:", &[AuxValue::Bytes(config.to_vec())])
        .map_err(|e| anyhow::anyhow!("setConfig: {e}"))?;
    conn.call_async(tap, "start", &[])
        .map_err(|e| anyhow::anyhow!("start: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            let _ = conn.call_async(tap, "stop", &[]);
            return Err(anyhow::anyhow!("no snapshot within 20s"));
        }
        match data_rx.recv_timeout(rem.min(Duration::from_secs(15))) {
            Ok(msg) => {
                if msg.expects_reply { let _ = conn.ack(&msg); }
                if let Some(snap) = extract_snapshot(&msg) {
                    let _ = conn.call_async(tap, "stop", &[]);
                    return Ok(snap);
                }
            }
            Err(_) => {
                let _ = conn.call_async(tap, "stop", &[]);
                return Err(anyhow::anyhow!("connection closed without snapshot"));
            }
        }
    }
}

/// Find which index in the device-reported attr list corresponds to a field name.
/// Used at parse time to handle arbitrary attribute ordering.
fn attr_index(schema: &[Value], name: &str) -> Option<usize> {
    schema.iter().position(|v| v.as_string() == Some(name))
}

// ── connect ───────────────────────────────────────────────────────────────────

fn connect_hub(session: &mut ios_rs::tunnel::DeviceSession) -> Result<Arc<DtxConn>> {
    if !session.is_rsd() {
        if let Ok(socket) = session.lockdown()
            .connect_service("com.apple.instruments.dtservicehub")
        {
            let r = socket.try_clone().map_err(|e| anyhow::anyhow!("clone: {e}"))?;
            return Ok(Arc::new(DtxConn::new(r, socket)));
        }
    }

    let stream = session
        .connect_rsd_service("com.apple.instruments.dtservicehub")
        .map_err(|e| anyhow::anyhow!(
            "dtservicehub (is Developer Mode enabled?): {e}"
        ))?;
    let stream_r = stream.try_clone()
        .map_err(|e| anyhow::anyhow!("stream clone: {e}"))?;
    Ok(Arc::new(DtxConn::new(stream_r, stream)))
}

// ── config builder ────────────────────────────────────────────────────────────

fn build_config(interval_ms: u64, proc_attrs: &[&str], sys_attrs: &[&str]) -> Vec<u8> {
    let ns = (interval_ms * 1_000_000) as i64;
    let mut objects: Vec<Value> = vec![Value::String("$null".into())];

    let cls_dict  = push_class(&mut objects, "NSMutableDictionary", &["NSDictionary", "NSObject"]);
    let cls_array = push_class(&mut objects, "NSArray", &["NSObject"]);

    let v_ur   = push_val(&mut objects, Value::Integer(1.into()));
    let v_bm   = push_val(&mut objects, Value::Integer(0.into()));
    let v_si   = push_val(&mut objects, Value::Integer(ns.into()));
    let v_true = push_val(&mut objects, Value::Boolean(true));

    let make_arr = |objects: &mut Vec<Value>, strs: &[&str]| -> plist::Uid {
        let uids: Vec<plist::Uid> = strs.iter().map(|s| push_val(objects, Value::String((*s).into()))).collect();
        let idx = plist::Uid::new(objects.len() as u64);
        let mut d = Dictionary::new();
        d.insert("NS.objects".into(), Value::Array(uids.iter().map(|u| Value::Uid(*u)).collect()));
        d.insert("$class".into(), Value::Uid(cls_array));
        objects.push(Value::Dictionary(d));
        idx
    };

    let v_proc = make_arr(&mut objects, proc_attrs);
    let v_sys  = make_arr(&mut objects, sys_attrs);

    let k_ur   = push_val(&mut objects, Value::String("ur".into()));
    let k_bm   = push_val(&mut objects, Value::String("bm".into()));
    let k_si   = push_val(&mut objects, Value::String("sampleInterval".into()));
    let k_cpu  = push_val(&mut objects, Value::String("cpuUsage".into()));
    let k_phys = push_val(&mut objects, Value::String("physFootprint".into()));
    let k_proc = push_val(&mut objects, Value::String("procAttrs".into()));
    let k_sys  = push_val(&mut objects, Value::String("sysAttrs".into()));

    let root = plist::Uid::new(objects.len() as u64);
    {
        let mut d = Dictionary::new();
        d.insert("NS.keys".into(),    Value::Array([k_ur, k_bm, k_si, k_cpu, k_phys, k_proc, k_sys].iter().map(|u| Value::Uid(*u)).collect()));
        d.insert("NS.objects".into(), Value::Array([v_ur, v_bm, v_si, v_true, v_true, v_proc, v_sys].iter().map(|u| Value::Uid(*u)).collect()));
        d.insert("$class".into(), Value::Uid(cls_dict));
        objects.push(Value::Dictionary(d));
    }

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(root));
    let mut r = Dictionary::new();
    r.insert("$version".into(),  Value::Integer(100000.into()));
    r.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    r.insert("$top".into(),      Value::Dictionary(top));
    r.insert("$objects".into(),  Value::Array(objects));

    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &Value::Dictionary(r)).unwrap();
    buf
}

fn push_class(objects: &mut Vec<Value>, name: &str, supers: &[&str]) -> plist::Uid {
    let idx = plist::Uid::new(objects.len() as u64);
    let mut classes: Vec<Value> = vec![Value::String(name.into())];
    classes.extend(supers.iter().map(|s| Value::String((*s).into())));
    let mut d = Dictionary::new();
    d.insert("$classname".into(), Value::String(name.into()));
    d.insert("$classes".into(),   Value::Array(classes));
    objects.push(Value::Dictionary(d));
    idx
}

fn push_val(objects: &mut Vec<Value>, v: Value) -> plist::Uid {
    let idx = plist::Uid::new(objects.len() as u64);
    objects.push(v);
    idx
}

// ── snapshot parsing ──────────────────────────────────────────────────────────

fn extract_snapshot(msg: &dtx::DtxMessage) -> Option<Snapshot> {
    if let Some(v) = &msg.payload {
        if let Some(s) = try_parse_snapshot(v)        { return Some(s); }
        if let Some(s) = nska_extract_snapshot(v)     { return Some(s); }
    }
    for aux in &msg.aux {
        if let dtx::AuxValue::Bytes(bytes) = aux {
            if let Ok(v) = plist::from_bytes::<Value>(bytes) {
                if let Some(s) = try_parse_snapshot(&v)    { return Some(s); }
                if let Some(s) = nska_extract_snapshot(&v) { return Some(s); }
            }
        }
    }
    None
}

fn nska_extract_snapshot(v: &Value) -> Option<Snapshot> {
    let objects = v.as_dictionary()?.get("$objects")?.as_array()?;
    let root_uid = v.as_dictionary()?
        .get("$top")?.as_dictionary()?
        .get("root")?.as_uid()?.get() as usize;
    let decoded = nska_decode_value(objects.get(root_uid)?, objects);
    match &decoded {
        Value::Dictionary(_) => try_parse_snapshot(&decoded),
        Value::Array(arr) => {
            for item in arr { if let Some(s) = try_parse_snapshot(item) { return Some(s); } }
            nska_try_array(objects.get(root_uid)?, objects)
        }
        _ => None,
    }
}

fn nska_try_array(v: &Value, objects: &[Value]) -> Option<Snapshot> {
    let arr = v.as_dictionary()?.get("NS.objects")?.as_array()?;
    for item in arr {
        let idx = item.as_uid()?.get() as usize;
        let decoded = nska_decode_value(objects.get(idx)?, objects);
        if let Some(s) = try_parse_snapshot(&decoded) { return Some(s); }
    }
    None
}

fn try_parse_snapshot(v: &Value) -> Option<Snapshot> {
    let dict = v.as_dictionary()?;
    if !dict.contains_key("SystemCPUUsage") && !dict.contains_key("Processes") { return None; }
    // NOTE: On iOS 26, sysmontap sends only system data (Type=43). The Processes key
    // is absent — per-process data is not available via this channel on iOS 26.

    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

    let cpu_total = dict.get("SystemCPUUsage")
        .and_then(|v| v.as_dictionary())
        .and_then(|d| d.get("CPU_TotalLoad"))
        .and_then(|v| v.as_real())
        .unwrap_or(0.0).max(0.0);

    const PAGE: u64 = 16_384;
    let (phys_mem, mem_used) = {
        let schema = dict.get("SystemAttributes").and_then(|v| v.as_array());
        let vals   = dict.get("System").and_then(|v| v.as_array());
        let lookup = |key: &str| -> u64 {
            schema.as_ref().zip(vals.as_ref())
                .and_then(|(s, v)| attr_index(s, key)
                    .and_then(|i| v.get(i))
                    .and_then(|v| v.as_unsigned_integer()))
                .unwrap_or(0)
        };
        let phys = lookup("physMemSize");
        let used = lookup("vmUsedCount");
        let free = lookup("vmFreeCount");
        let phys_b = if phys > 0 { phys * PAGE } else {
            dict.get("physMemory").and_then(|v| v.as_unsigned_integer()).unwrap_or(0)
        };
        let used_b = if used + free > 0 { used * PAGE } else { 0 };
        (phys_b, used_b)
    };

    // Parse Processes dict: keys are PID strings, values are arrays indexed by ProcessesAttributes.
    let schema = dict.get("ProcessesAttributes").and_then(|v| v.as_array());
    let proc_dict = dict.get("Processes").and_then(|v| v.as_dictionary());
    let processes: Vec<ProcSnap> = match (schema, proc_dict) {
        (Some(schema), Some(procs)) => {
            let i_cpu  = attr_index(schema, "cpuUsage").unwrap_or(1);
            let i_phys = attr_index(schema, "physFootprint")
                .or_else(|| attr_index(schema, "memPhysFootprint")).unwrap_or(4);
            let i_virt = attr_index(schema, "memVirtualSize").unwrap_or(0);
            let i_pid  = attr_index(schema, "pid").unwrap_or(usize::MAX);
            let i_name = attr_index(schema, "name").unwrap_or(usize::MAX);

            let mut out: Vec<ProcSnap> = procs.values().filter_map(|val| {
                let arr  = val.as_array()?;
                let cpu  = arr.get(i_cpu).and_then(real_or_int).unwrap_or(0.0);
                let phys = arr.get(i_phys).and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
                let virt = arr.get(i_virt).and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
                let pid  = if i_pid < arr.len() { arr[i_pid].as_unsigned_integer().unwrap_or(0) as u32 } else { 0 };
                let name = if i_name < arr.len() { arr[i_name].as_string().unwrap_or("?").to_string() } else { "?".to_string() };
                Some(ProcSnap { pid, name, cpu, phys, virt })
            }).collect();
            out.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
            out
        }
        _ => vec![],
    };

    Some(Snapshot { ts, cpu_total, phys_mem, mem_used, processes })
}

fn real_or_int(v: &Value) -> Option<f64> {
    v.as_real()
        .or_else(|| v.as_signed_integer().map(|i| i as f64))
        .or_else(|| v.as_unsigned_integer().map(|u| u as f64))
}

fn nska_decode_value(v: &Value, objects: &[Value]) -> Value {
    match v {
        Value::Uid(uid) => objects.get(uid.get() as usize)
            .map(|o| nska_decode_value(o, objects))
            .unwrap_or(Value::String("$null".into())),
        Value::Dictionary(d) => {
            if d.contains_key("NS.keys") && d.contains_key("NS.objects") {
                let keys = d.get("NS.keys").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let vals = d.get("NS.objects").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let mut out = plist::Dictionary::new();
                for (k, val) in keys.iter().zip(vals.iter()) {
                    let ks = match nska_decode_value(k, objects) { Value::String(s) => s, o => format!("{o:?}") };
                    out.insert(ks, nska_decode_value(val, objects));
                }
                return Value::Dictionary(out);
            }
            if let Some(arr) = d.get("NS.objects").and_then(|v| v.as_array()) {
                return Value::Array(arr.iter().map(|i| nska_decode_value(i, objects)).collect());
            }
            let mut out = plist::Dictionary::new();
            for (k, val) in d { if k != "$class" { out.insert(k.clone(), nska_decode_value(val, objects)); } }
            Value::Dictionary(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|i| nska_decode_value(i, objects)).collect()),
        other => other.clone(),
    }
}

// ── display ───────────────────────────────────────────────────────────────────

fn print_htop(snap: &Snapshot, cols: usize, rows: usize) {
    let mut out = String::with_capacity(4096);
    out.push_str("\x1b[H\x1b[2J");

    let secs  = snap.ts % 86400;
    out.push_str(&format!("\x1b[1miOS Performance Monitor  [{:02}:{:02}:{:02}]\x1b[0m\n",
        secs / 3600, (secs % 3600) / 60, secs % 60));

    let bar_w  = cols.saturating_sub(20).min(40);
    let cpu_pc = snap.cpu_total.clamp(0.0, 100.0);
    let f1     = ((cpu_pc / 100.0) * bar_w as f64) as usize;
    out.push_str(&format!("CPU: [{}{}] {:5.1}%\n",
        "█".repeat(f1), "░".repeat(bar_w.saturating_sub(f1)), cpu_pc));

    if snap.phys_mem > 0 {
        let used   = if snap.mem_used > 0 { snap.mem_used }
                     else { snap.processes.iter().map(|p| p.phys).sum() };
        let mem_pc = (used as f64 / snap.phys_mem as f64 * 100.0).clamp(0.0, 100.0);
        let f2     = ((mem_pc / 100.0) * bar_w as f64) as usize;
        out.push_str(&format!("MEM: [{}{}] {} / {}\n",
            "█".repeat(f2), "░".repeat(bar_w.saturating_sub(f2)),
            fmt_bytes(used), fmt_bytes(snap.phys_mem)));
    }

    let name_w = cols.saturating_sub(42).clamp(10, 28);
    out.push_str(&format!("\n\x1b[7m {:>6}  {:<name_w$}  {:>6}  {:>8}  {:>8}\x1b[0m\n",
        "PID", "Name", "CPU%", "MEM", "VIRT", name_w = name_w));

    for p in snap.processes.iter().take(rows.saturating_sub(7).max(1)) {
        let name = if p.name.len() > name_w { format!("{}…", &p.name[..name_w.saturating_sub(1)]) }
                   else { p.name.clone() };
        out.push_str(&format!(" {:>6}  {:<name_w$}  {:>5.1}%  {:>8}  {:>8}\n",
            p.pid, name, p.cpu * 100.0, fmt_bytes(p.phys), fmt_bytes(p.virt),
            name_w = name_w));
    }

    print!("{}", out);
    std::io::stdout().flush().ok();
}

fn print_status(msg: &str, _cols: usize) {
    print!("\x1b[H\x1b[2J\x1b[1miOS Performance Monitor\x1b[0m\n{}\n", msg);
    std::io::stdout().flush().ok();
}

fn print_json(snap: &Snapshot) {
    let procs: Vec<String> = snap.processes.iter().map(|p| {
        format!(r#"{{"pid":{},"name":{},"cpu_pct":{:.2},"mem_bytes":{},"virt_bytes":{}}}"#,
            p.pid, json_str(&p.name), p.cpu * 100.0, p.phys, p.virt)
    }).collect();
    println!(r#"{{"ts":{},"cpu_total_pct":{:.2},"phys_mem_bytes":{},"mem_used_bytes":{},"processes":[{}]}}"#,
        snap.ts, snap.cpu_total, snap.phys_mem, snap.mem_used, procs.join(","));
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1 << 30      { format!("{:.1}G", n as f64 / (1u64 << 30) as f64) }
    else if n >= 1 << 20 { format!("{:.0}M", n as f64 / (1u64 << 20) as f64) }
    else if n >= 1 << 10 { format!("{:.0}K", n as f64 / (1u64 << 10) as f64) }
    else                 { format!("{}B", n) }
}

fn term_size() -> (usize, usize) {
    let cols = std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()).unwrap_or(80usize);
    let rows = std::env::var("LINES").ok().and_then(|s| s.parse().ok()).unwrap_or(24usize);
    (cols, rows)
}

struct CursorGuard;
impl Drop for CursorGuard {
    fn drop(&mut self) { print!("\x1b[?25h"); std::io::stdout().flush().ok(); }
}
