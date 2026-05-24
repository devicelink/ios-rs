use std::collections::HashMap;
use std::net::TcpListener;
use std::thread;

use anyhow::{anyhow, Result};
use ios_rs::tunnel::DeviceSession;
use ios_rs::xpc::Value;

pub fn run(session: &mut DeviceSession, bundle_id: &str, terminate_existing: bool) -> Result<i64> {
    let xpc_conn = connect_appservice(session)?;

    // Empty binary-plist dict for platformSpecificOptions
    let mut plat_buf = Vec::new();
    plist::to_writer_binary(
        &mut plat_buf,
        &plist::Value::Dictionary(plist::Dictionary::new()),
    )
    .unwrap();

    let device_id = random_uuid_string();
    let input = build_launch_input(bundle_id, terminate_existing, &plat_buf);
    let payload = build_coredevice_request(
        &device_id,
        "com.apple.coredevice.feature.launchapplication",
        Some(input),
    );

    let reply = xpc_conn
        .request(payload)
        .map_err(|e| anyhow!("appservice request: {e}"))?;

    let pid = reply
        .as_dict()
        .and_then(|d| d.get("CoreDevice.output"))
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("processToken"))
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("processIdentifier"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("no PID in appservice reply: {reply:?}"))?;

    Ok(pid)
}

// ── helpers ───────────────────────────────────────────────────────────────────

pub fn connect_appservice(session: &mut DeviceSession) -> Result<ios_rs::remotexpc::RemoteXpcConn> {
    let stream = session
        .connect_rsd_service("com.apple.coredevice.appservice")
        .map_err(|e| anyhow!("coredevice.appservice: {e}"))?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let relay_addr = listener.local_addr()?;
    thread::spawn(move || {
        if let Ok((server, _)) = listener.accept() {
            let mut r = stream.try_clone().unwrap();
            let mut w = stream;
            let mut tw = server.try_clone().unwrap();
            let mut tr = server;
            let t1 = thread::spawn(move || {
                std::io::copy(&mut r, &mut tw).ok();
            });
            let t2 = thread::spawn(move || {
                std::io::copy(&mut tr, &mut w).ok();
            });
            let _ = (t1.join(), t2.join());
        }
    });

    ios_rs::remotexpc::RemoteXpcConn::connect(relay_addr)
        .map_err(|e| anyhow!("remotexpc connect: {e}"))
}

fn build_launch_input(bundle_id: &str, terminate_existing: bool, platform_opts: &[u8]) -> Value {
    let mut d: HashMap<String, Value> = HashMap::new();
    d.insert("applicationSpecifier".into(), {
        let mut spec: HashMap<String, Value> = HashMap::new();
        let mut bi: HashMap<String, Value> = HashMap::new();
        bi.insert("_0".into(), Value::String(bundle_id.into()));
        spec.insert("bundleIdentifier".into(), Value::Dictionary(bi));
        Value::Dictionary(spec)
    });
    d.insert("options".into(), {
        let mut o: HashMap<String, Value> = HashMap::new();
        o.insert("arguments".into(), Value::Array(vec![]));
        o.insert(
            "environmentVariables".into(),
            Value::Dictionary(HashMap::new()),
        );
        o.insert(
            "platformSpecificOptions".into(),
            Value::Data(platform_opts.to_vec()),
        );
        o.insert("standardIOUsesPseudoterminals".into(), Value::Bool(true));
        o.insert("startStopped".into(), Value::Bool(false));
        o.insert("terminateExisting".into(), Value::Bool(terminate_existing));
        o.insert("user".into(), {
            let mut u: HashMap<String, Value> = HashMap::new();
            u.insert("active".into(), Value::Bool(true));
            Value::Dictionary(u)
        });
        o.insert("workingDirectory".into(), Value::Null);
        Value::Dictionary(o)
    });
    d.insert(
        "standardIOIdentifiers".into(),
        Value::Dictionary(HashMap::new()),
    );
    Value::Dictionary(d)
}

pub fn build_coredevice_request(device_id: &str, feature: &str, input: Option<Value>) -> Value {
    let mut d: HashMap<String, Value> = HashMap::new();
    d.insert(
        "CoreDevice.CoreDeviceDDIProtocolVersion".into(),
        Value::Int64(0),
    );
    d.insert(
        "CoreDevice.action".into(),
        Value::Dictionary(HashMap::new()),
    );
    d.insert("CoreDevice.coreDeviceVersion".into(), {
        let mut v: HashMap<String, Value> = HashMap::new();
        v.insert("stringValue".into(), Value::String("348.1".into()));
        v.insert("originalComponentsCount".into(), Value::Int64(2));
        v.insert(
            "components".into(),
            Value::Array(vec![
                Value::Uint64(348),
                Value::Uint64(1),
                Value::Uint64(0),
                Value::Uint64(0),
                Value::Uint64(0),
            ]),
        );
        Value::Dictionary(v)
    });
    d.insert(
        "CoreDevice.deviceIdentifier".into(),
        Value::String(device_id.into()),
    );
    d.insert(
        "CoreDevice.featureIdentifier".into(),
        Value::String(feature.into()),
    );
    d.insert(
        "CoreDevice.invocationIdentifier".into(),
        Value::String(random_uuid_string()),
    );
    d.insert("CoreDevice.input".into(), input.unwrap_or(Value::Null));
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
