use std::sync::Arc;

use anyhow::{Result, anyhow};
use ios_rs::dtx::{DtxConn, AuxValue};
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use plist::Value;

use crate::cmd::open_session;

pub fn run(udid: Option<&str>) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let procs = list_processes(&mut session)?;

    if procs.is_empty() {
        eprintln!("no processes returned (developer mode required on iOS 17+)");
        return Ok(());
    }

    let name_w = procs.iter().map(|p| p.name.len()).max().unwrap_or(10).clamp(10, 40);
    println!("{:>6}  {:<name_w$}  {}", "PID", "Name", "Bundle ID / Type", name_w = name_w);
    println!("{}", "-".repeat(6 + 2 + name_w + 2 + 20));
    for p in &procs {
        let bundle = if p.real_app_name.is_empty() {
            if p.is_application { "app".into() } else { "daemon".into() }
        } else {
            p.real_app_name.clone()
        };
        println!("{:>6}  {:<name_w$}  {}", p.pid, p.name, bundle, name_w = name_w);
    }
    println!("({} processes)", procs.len());
    Ok(())
}

// ── data model ────────────────────────────────────────────────────────────────

pub struct ProcessInfo {
    pub pid:           u64,
    pub name:          String,
    pub real_app_name: String,
    pub is_application: bool,
}

// ── DTX helpers ───────────────────────────────────────────────────────────────

fn list_processes(session: &mut DeviceSession) -> Result<Vec<ProcessInfo>> {
    let conn = connect_hub(session)?;
    conn.handshake().map_err(|e| anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.deviceinfo")
        .map_err(|e| anyhow!("deviceinfo channel: {e}"))?;

    let reply = conn
        .call_full(ch, "runningProcesses", &[])
        .map_err(|e| anyhow!("runningProcesses: {e}"))?;

    // Parse from payload body or aux[0] bytes — both are NSKeyedArchiver-encoded arrays.
    let raw = if let Some(v) = &reply.payload {
        Some(v.clone())
    } else {
        reply.aux.iter().find_map(|a| {
            if let AuxValue::Bytes(b) = a {
                plist::from_bytes::<Value>(b).ok()
            } else { None }
        })
    };

    let v = raw.ok_or_else(|| anyhow!("runningProcesses returned no data"))?;
    parse_process_list(&v)
}

fn parse_process_list(v: &Value) -> Result<Vec<ProcessInfo>> {
    // Decode NSKeyedArchiver to get the array at $top.root
    let arr = nska_root_array(v)
        .ok_or_else(|| anyhow!("unexpected runningProcesses response shape"))?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Value::Dictionary(d) = item {
            let pid = d.get("pid").and_then(|v| v.as_unsigned_integer()).unwrap_or(0);
            let name = d.get("name").and_then(|v| v.as_string()).unwrap_or("").to_string();
            let real_app_name = d.get("realAppName").and_then(|v| v.as_string()).unwrap_or("").to_string();
            let is_application = d.get("isApplication").and_then(|v| v.as_boolean()).unwrap_or(false);
            out.push(ProcessInfo { pid, name, real_app_name, is_application });
        }
    }
    out.sort_by_key(|p| p.pid);
    Ok(out)
}

/// Decode the root object from an NSKeyedArchiver plist and return it as a
/// Vec<Value> if it's an NS(Mutable)Array.
fn nska_root_array(v: &Value) -> Option<Vec<Value>> {
    let d = v.as_dictionary()?;
    let objects = d.get("$objects")?.as_array()?;
    let root_uid = d.get("$top")?.as_dictionary()?.get("root")?.as_uid()?.get() as usize;
    let root = objects.get(root_uid)?;
    Some(nska_decode_array(root, objects))
}

fn nska_decode_value(v: &Value, objects: &[Value]) -> Value {
    match v {
        Value::Uid(uid) => objects
            .get(uid.get() as usize)
            .map(|o| nska_decode_value(o, objects))
            .unwrap_or(Value::String("$null".into())),
        Value::Dictionary(d) => {
            if d.contains_key("NS.keys") && d.contains_key("NS.objects") {
                let keys = d.get("NS.keys").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let vals = d.get("NS.objects").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let mut out = plist::Dictionary::new();
                for (k, val) in keys.iter().zip(vals.iter()) {
                    let ks = match nska_decode_value(k, objects) {
                        Value::String(s) => s,
                        other => format!("{other:?}"),
                    };
                    out.insert(ks, nska_decode_value(val, objects));
                }
                Value::Dictionary(out)
            } else if let Some(arr) = d.get("NS.objects").and_then(|v| v.as_array()) {
                Value::Array(arr.iter().map(|i| nska_decode_value(i, objects)).collect())
            } else {
                let mut out = plist::Dictionary::new();
                for (k, val) in d {
                    if k != "$class" {
                        out.insert(k.clone(), nska_decode_value(val, objects));
                    }
                }
                Value::Dictionary(out)
            }
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|i| nska_decode_value(i, objects)).collect()),
        other => other.clone(),
    }
}

fn nska_decode_array(root: &Value, objects: &[Value]) -> Vec<Value> {
    let decoded = nska_decode_value(root, objects);
    match decoded {
        Value::Array(arr) => arr,
        // NSArray stored as dict with NS.objects
        Value::Dictionary(ref d) if d.contains_key("NS.objects") => {
            let arr = d.get("NS.objects").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            arr.iter().map(|i| nska_decode_value(i, objects)).collect()
        }
        _ => vec![],
    }
}

fn connect_hub(session: &mut DeviceSession) -> Result<Arc<DtxConn>> {
    let rsd = session.connect_rsd().map_err(|e| anyhow!("RSD: {e}"))?;
    let port = rsd
        .service("com.apple.instruments.dtservicehub")
        .ok_or_else(|| anyhow!("dtservicehub not in RSD catalog — is Developer Mode enabled?"))?
        .port;
    let tunnel = session.smoltcp_tunnel_ref().ok_or_else(|| anyhow!("no CDTunnel"))?;
    let stream = tunnel.connect(tunnel.params.server_addr, port)
        .map_err(|e| anyhow!("connect dtservicehub: {e}"))?;
    let stream_r = stream.try_clone().map_err(|e| anyhow!("stream clone: {e}"))?;
    Ok(Arc::new(DtxConn::new(stream_r, stream)))
}
