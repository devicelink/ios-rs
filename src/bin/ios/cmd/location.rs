use std::sync::Arc;

use anyhow::{anyhow, Result};
use ios_rs::dtx::{archive_primitive, AuxValue, DtxConn};
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use plist::Value;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

pub fn set(udid: Option<&str>, lat: f64, lon: f64, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let conn = connect_hub(&mut session)?;
    conn.handshake().map_err(|e| anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.LocationSimulation")
        .map_err(|e| anyhow!("LocationSimulation channel: {e}"))?;

    conn.call(
        ch,
        "simulateLocationWithLatitude:longitude:",
        &[
            AuxValue::Bytes(archive_primitive(Value::Real(lat))),
            AuxValue::Bytes(archive_primitive(Value::Real(lon))),
        ],
    )
    .map_err(|e| anyhow!("simulateLocation: {e}"))?;

    if output.is_json() {
        print_json(&ActionResult::with_msg(format!(
            "location set to {lat:.6}, {lon:.6}"
        )))?;
    } else {
        eprintln!("location set to {lat:.6}, {lon:.6}");
    }
    Ok(())
}

pub fn clear(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let conn = connect_hub(&mut session)?;
    conn.handshake().map_err(|e| anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.LocationSimulation")
        .map_err(|e| anyhow!("LocationSimulation channel: {e}"))?;

    conn.call(ch, "stopLocationSimulation", &[])
        .map_err(|e| anyhow!("stopLocationSimulation: {e}"))?;

    if output.is_json() {
        print_json(&ActionResult::with_msg("location simulation cleared"))?;
    } else {
        eprintln!("location simulation cleared");
    }
    Ok(())
}

fn connect_hub(session: &mut DeviceSession) -> Result<Arc<DtxConn>> {
    let stream = session
        .connect_rsd_service("com.apple.instruments.dtservicehub")
        .map_err(|e| anyhow!("dtservicehub (is Developer Mode enabled?): {e}"))?;
    let stream_r = stream
        .try_clone()
        .map_err(|e| anyhow!("stream clone: {e}"))?;
    Ok(Arc::new(DtxConn::new(stream_r, stream)))
}
