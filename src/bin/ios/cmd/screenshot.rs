use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use ios_rs::dtx::{AuxValue, DtxConn};
use ios_rs::lockdown::services::screenshot::ScreenshotClient;
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use plist::Value;

use crate::cmd::open_session;

pub fn run(udid: Option<&str>, mode: ConnectionMode, output: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;

    let png = if session.is_rsd() {
        take_modern(&mut session).context("screenshot via dtservicehub")?
    } else {
        let mut client = ScreenshotClient::connect(session.lockdown())
            .context("connect screenshotr (legacy, iOS < 17)")?;
        client.take().context("take screenshot")?
    };

    if output == "-" {
        std::io::stdout().write_all(&png).context("write PNG to stdout")?;
    } else {
        std::fs::write(output, &png)
            .with_context(|| format!("write {output}"))?;
        eprintln!("saved {} bytes → {output}", png.len());
    }
    Ok(())
}

// ── modern path (iOS 17.4+) ───────────────────────────────────────────────────

fn take_modern(session: &mut DeviceSession) -> Result<Vec<u8>> {
    let conn = connect_hub(session)?;
    conn.handshake().map_err(|e| anyhow::anyhow!("dtservicehub handshake: {e}"))?;

    // Open deviceinfo first — required prerequisite on some iOS versions (mirrors perf.rs).
    if let Ok(di) = conn.request_channel("com.apple.instruments.server.services.deviceinfo") {
        let _ = conn.call_async(di, "sysmonProcessAttributes", &[]);
        let _ = conn.call_async(di, "sysmonSystemAttributes", &[]);
    }

    let ch = conn
        .request_channel("com.apple.instruments.server.services.screenshot")
        .map_err(|e| anyhow::anyhow!("screenshot channel: {e}"))?;

    // takeScreenshot returns a direct reply; the PNG is in the first aux entry.
    let reply = conn
        .call_full(ch, "takeScreenshot", &[])
        .map_err(|e| anyhow::anyhow!("takeScreenshot: {e} — note: requires Developer Mode on iOS 17+"))?;

    // PNG arrives as raw bytes in aux[0] (go-ios: msg.Payload[0])
    for aux in &reply.aux {
        if let AuxValue::Bytes(bytes) = aux {
            if let Some(png) = extract_png_bytes(bytes) {
                return Ok(png);
            }
        }
    }
    // Fallback: check the plist body too
    if let Some(v) = &reply.payload {
        if let Some(png) = extract_png_value(v) {
            return Ok(png);
        }
    }
    Err(anyhow::anyhow!(
        "takeScreenshot: no PNG found in reply (aux={} payload={:?})",
        reply.aux.len(),
        reply.payload,
    ))
}

fn connect_hub(session: &mut DeviceSession) -> Result<Arc<DtxConn>> {
    let rsd = session
        .connect_rsd()
        .map_err(|e| anyhow::anyhow!("RSD: {e}"))?;
    let port = rsd
        .service("com.apple.instruments.dtservicehub")
        .ok_or_else(|| {
            anyhow::anyhow!("dtservicehub not in RSD catalog — is Developer Mode enabled?")
        })?
        .port;

    let tunnel = session
        .smoltcp_tunnel_ref()
        .ok_or_else(|| anyhow::anyhow!("no CDTunnel"))?;
    let stream = tunnel
        .connect(tunnel.params.server_addr, port)
        .map_err(|e| anyhow::anyhow!("connect dtservicehub:{port}: {e}"))?;
    let stream_r = stream
        .try_clone()
        .map_err(|e| anyhow::anyhow!("stream clone: {e}"))?;
    Ok(Arc::new(DtxConn::new(stream_r, stream)))
}

/// Try to extract PNG from raw bytes: direct PNG, or NSKeyedArchiver-wrapped NSData.
fn extract_png_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"\x89PNG") {
        return Some(bytes.to_vec());
    }
    // May be NSKeyedArchiver binary plist containing NSData
    if let Ok(v) = plist::from_bytes::<Value>(bytes) {
        return extract_png_value(&v);
    }
    None
}

/// Extract PNG from a plist Value (direct Data or NSKeyedArchiver dict).
fn extract_png_value(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Data(bytes) => extract_png_bytes(bytes),
        Value::Dictionary(d)
            if d.get("$archiver").and_then(|v| v.as_string()) == Some("NSKeyedArchiver") =>
        {
            let objects = d.get("$objects")?.as_array()?;
            let root = d
                .get("$top")?
                .as_dictionary()?
                .get("root")?
                .as_uid()?
                .get() as usize;
            extract_png_value(objects.get(root)?)
        }
        _ => None,
    }
}
