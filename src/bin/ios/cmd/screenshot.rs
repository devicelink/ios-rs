use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use ios_rs::dtx::{AuxValue, DtxConn};
use ios_rs::lockdown::services::screenshot::ScreenshotClient;
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use plist::Value;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct ScreenshotResult {
    ok: bool,
    path: String,
    bytes: u64,
}

pub fn run(
    udid: Option<&str>,
    mode: ConnectionMode,
    output_path: &str,
    output: OutputMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;

    let png = if session.is_rsd() {
        take_modern(&mut session)?
    } else {
        let mut client = ScreenshotClient::connect(session.lockdown())
            .context("connect screenshotr (legacy, iOS < 17)")?;
        client.take().context("take screenshot")?
    };

    if output_path == "-" {
        std::io::stdout()
            .write_all(&png)
            .context("write PNG to stdout")?;
        if output.is_json() {
            // Can't emit JSON if we wrote raw PNG to stdout
        }
    } else {
        std::fs::write(output_path, &png).with_context(|| format!("write {output_path}"))?;
        if output.is_json() {
            return print_json(&ScreenshotResult {
                ok: true,
                path: output_path.to_string(),
                bytes: png.len() as u64,
            });
        }
        eprintln!("saved {} bytes → {output_path}", png.len());
    }
    Ok(())
}

// ── modern path (iOS 17.4+) ───────────────────────────────────────────────────

fn take_modern(session: &mut DeviceSession) -> Result<Vec<u8>> {
    let conn = connect_hub(session)?;
    conn.handshake()
        .map_err(|e| anyhow::anyhow!("handshake: {e}"))?;

    let ch = conn
        .request_channel("com.apple.instruments.server.services.screenshot")
        .map_err(|e| anyhow::anyhow!("screenshot channel: {e}"))?;

    let reply = conn
        .call_full(ch, "takeScreenshot", &[])
        .map_err(|e| anyhow::anyhow!("takeScreenshot: {e}"))?;

    for aux in &reply.aux {
        if let AuxValue::Bytes(bytes) = aux {
            if let Some(png) = extract_png_bytes(bytes) {
                return Ok(png);
            }
        }
    }
    if let Some(v) = &reply.payload {
        if let Some(png) = extract_png_value(v) {
            return Ok(png);
        }
    }
    Err(anyhow::anyhow!(
        "takeScreenshot: no PNG in reply (aux={} payload={:?})",
        reply.aux.len(),
        reply.payload,
    ))
}

fn connect_hub(session: &mut DeviceSession) -> Result<Arc<DtxConn>> {
    let stream = session
        .connect_rsd_service("com.apple.instruments.dtservicehub")
        .map_err(|e| anyhow::anyhow!("dtservicehub: {e}"))?;
    let stream_r = stream
        .try_clone()
        .map_err(|e| anyhow::anyhow!("stream clone: {e}"))?;
    Ok(Arc::new(DtxConn::new(stream_r, stream)))
}

fn extract_png_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"\x89PNG") {
        return Some(bytes.to_vec());
    }
    if let Ok(v) = plist::from_bytes::<Value>(bytes) {
        return extract_png_value(&v);
    }
    None
}

fn extract_png_value(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Data(bytes) => extract_png_bytes(bytes),
        Value::Dictionary(d)
            if d.get("$archiver").and_then(|v| v.as_string()) == Some("NSKeyedArchiver") =>
        {
            let objects = d.get("$objects")?.as_array()?;
            let root = d.get("$top")?.as_dictionary()?.get("root")?.as_uid()?.get() as usize;
            extract_png_value(objects.get(root)?)
        }
        _ => None,
    }
}
