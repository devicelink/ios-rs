//! Darwin notification proxy via `com.apple.mobile.insecure_notification_proxy.shim.remote`.
//!
//! Allows posting and observing Darwin notifications on the device.
use std::io::{Read, Write};

use anyhow::{Context, Result};
use ios_rs::tunnel::ConnectionMode;
use ios_rs::usbmux::MuxSocket;
use plist::Value;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

const SHIM: &str = "com.apple.mobile.insecure_notification_proxy.shim.remote";

#[derive(serde::Serialize)]
struct NotificationEvent {
    name: String,
}

pub fn post(udid: Option<&str>, mode: ConnectionMode, name: &str, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut stream  = session.connect_rsd_shim(SHIM).context("connect notification proxy")?;
    send_cmd(&mut stream, "PostNotification", Some(name))?;

    if output.is_json() {
        print_json(&ActionResult::with_msg(format!("posted: {name}")))?;
    } else {
        eprintln!("posted: {name}");
    }
    Ok(())
}

pub fn observe(udid: Option<&str>, mode: ConnectionMode, name: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut stream  = session.connect_rsd_shim(SHIM).context("connect notification proxy")?;

    match name {
        Some(n) => send_cmd(&mut stream, "ObserveNotification", Some(n))?,
        None    => send_cmd(&mut stream, "ObserveAllNotifications", None)?,
    }

    if !output.is_json() {
        eprintln!("listening for notifications (Ctrl-C to stop)…");
    }

    loop {
        let msg = match recv_plist(&mut stream) {
            Ok(v)  => v,
            Err(_) => break,
        };
        if let Some(dict) = msg.as_dictionary() {
            let cmd  = dict.get("Command").and_then(|v| v.as_string()).unwrap_or("");
            let note = dict.get("Name").and_then(|v| v.as_string()).unwrap_or("");
            if cmd == "RelayNotification" {
                if output.is_json() {
                    let event = NotificationEvent { name: note.to_owned() };
                    println!("{}", serde_json::to_string(&event)?);
                } else {
                    println!("{note}");
                }
                let _ = std::io::stdout().flush();
            }
        }
    }
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn send_cmd(stream: &mut MuxSocket, command: &str, name: Option<&str>) -> Result<()> {
    let mut d = plist::Dictionary::new();
    d.insert("Command".into(), Value::String(command.into()));
    if let Some(n) = name {
        d.insert("Name".into(), Value::String(n.into()));
    }
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(d))?;
    let len = body.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

fn recv_plist(stream: &mut MuxSocket) -> Result<Value> {
    let mut len_buf = [0u8; 4];
    read_exact(stream, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 1024 * 1024 {
        anyhow::bail!("implausible notification length {len}");
    }
    let mut body = vec![0u8; len];
    read_exact(stream, &mut body)?;
    Ok(plist::from_bytes(&body)?)
}

fn read_exact(s: &mut MuxSocket, buf: &mut [u8]) -> Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match s.read(&mut buf[done..]) {
            Ok(0)  => anyhow::bail!("connection closed"),
            Ok(n)  => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
