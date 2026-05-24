use std::collections::HashMap;

use anyhow::{anyhow, Result};
use ios_rs::tunnel::DeviceSession;
use ios_rs::xpc::Value;

use super::launch::{build_coredevice_request, connect_appservice};

pub fn run(session: &mut DeviceSession, pid: i64) -> Result<()> {
    let xpc_conn = connect_appservice(session)?;

    let device_id = random_uuid_string();
    let input = build_kill_input(pid);
    let payload = build_coredevice_request(
        &device_id,
        "com.apple.coredevice.feature.sendsignaltoprocess",
        Some(input),
    );

    let reply = xpc_conn
        .request(payload)
        .map_err(|e| anyhow!("appservice kill request: {e}"))?;

    if let Some(err) = reply.as_dict().and_then(|d| d.get("CoreDevice.error")) {
        anyhow::bail!("device error: {err:?}");
    }

    Ok(())
}

fn build_kill_input(pid: i64) -> Value {
    const SIGKILL: i64 = 9;
    let mut d: HashMap<String, Value> = HashMap::new();
    d.insert("process".into(), {
        let mut p: HashMap<String, Value> = HashMap::new();
        p.insert("processIdentifier".into(), Value::Int64(pid));
        Value::Dictionary(p)
    });
    d.insert("signal".into(), Value::Int64(SIGKILL));
    Value::Dictionary(d)
}

fn random_uuid_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let b = t.to_le_bytes();
    format!("{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[0],b[1],b[2],b[3], b[4],b[5], (b[6]&0x0f)|0x40, b[7],
        (b[8]&0x3f)|0x80, b[9], b[10],b[11],b[12],b[13],b[14],b[15])
}
