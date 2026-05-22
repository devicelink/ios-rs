use std::sync::Arc;

use anyhow::{Result, anyhow};
use ios_rs::dtx::{AuxValue, DtxConn, archive_primitive};
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use plist::Value;

use crate::cmd::open_session;

pub fn set(udid: Option<&str>, lat: f64, lon: f64) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let conn = connect_hub(&mut session)?;
    conn.handshake().map_err(|e| anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.LocationSimulation")
        .map_err(|e| anyhow!("LocationSimulation channel: {e}"))?;

    conn.call(ch, "simulateLocationWithLatitude:longitude:", &[
        AuxValue::Bytes(archive_primitive(Value::Real(lat))),
        AuxValue::Bytes(archive_primitive(Value::Real(lon))),
    ]).map_err(|e| anyhow!("simulateLocation: {e}"))?;

    eprintln!("location set to {lat:.6}, {lon:.6}");
    Ok(())
}

pub fn clear(udid: Option<&str>) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let conn = connect_hub(&mut session)?;
    conn.handshake().map_err(|e| anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.LocationSimulation")
        .map_err(|e| anyhow!("LocationSimulation channel: {e}"))?;

    conn.call(ch, "stopLocationSimulation", &[])
        .map_err(|e| anyhow!("stopLocationSimulation: {e}"))?;

    eprintln!("location simulation cleared");
    Ok(())
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
