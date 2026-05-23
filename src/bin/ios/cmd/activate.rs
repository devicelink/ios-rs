use std::io::Read;

use anyhow::{Context, Result, bail};
use ios_rs::lockdown::services::activation::ActivationClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

const DRM_HANDSHAKE_URL: &str = "https://albert.apple.com/deviceservices/drmHandshake";
const ACTIVATION_URL:    &str = "https://albert.apple.com/deviceservices/deviceActivation";
const USER_AGENT: &str = "iOS Device Activator (MobileActivation-592.103.2)";

pub fn activate(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;

    // Quick pre-check via lockdownd — avoid hitting Apple if already activated.
    let state = session.lockdown()
        .get_value(None, "ActivationState")
        .map(|v| v.into_string().unwrap_or_default())
        .unwrap_or_default();
    if state == "Activated" {
        println!("device is already activated");
        return Ok(());
    }
    eprintln!("activation state: {state}");

    let mut client = ActivationClient::connect(session.lockdown())
        .context("connect activation service")?;

    // ── Step 1: get tunnel session info from device ───────────────────────────
    eprintln!("requesting tunnel session info from device…");
    let session_info = client.create_tunnel1_session_info()
        .context("CreateTunnel1SessionInfoRequest")?;

    // Serialize the session-info dict as XML plist to POST to Apple.
    let mut session_info_xml = Vec::new();
    plist::to_writer_xml(&mut session_info_xml, &plist::Value::Dictionary(session_info))
        .context("serialize session info")?;

    // ── Step 2: DRM handshake with Apple ─────────────────────────────────────
    eprintln!("DRM handshake with {DRM_HANDSHAKE_URL}…");
    let drm_resp = ureq::post(DRM_HANDSHAKE_URL)
        .set("Content-Type", "application/x-apple-plist")
        .set("Accept",       "application/xml")
        .set("User-Agent",   USER_AGENT)
        .send_bytes(&session_info_xml)
        .context("drmHandshake request")?;
    let drm_bytes = {
        let mut buf = Vec::new();
        drm_resp.into_reader().read_to_end(&mut buf)
            .context("read drmHandshake response")?;
        buf
    };

    // ── Step 3: create activation info on device ──────────────────────────────
    eprintln!("creating activation info on device…");
    let activation_info = client.create_activation_info(&drm_bytes)
        .context("CreateActivationInfoRequest")?;

    let mut activation_info_xml = Vec::new();
    plist::to_writer_xml(&mut activation_info_xml, &plist::Value::Dictionary(activation_info))
        .context("serialize activation info")?;

    // ── Step 4: activate with Apple ───────────────────────────────────────────
    eprintln!("activating with {ACTIVATION_URL}…");
    let body = format!("activation-info={}", percent_encode(&activation_info_xml));
    let apple_resp = ureq::post(ACTIVATION_URL)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept",       "*/*")
        .set("User-Agent",   USER_AGENT)
        .send_string(&body)
        .context("deviceActivation request")?;

    // Collect response headers before consuming the body.
    let mut headers = plist::Dictionary::new();
    for name in apple_resp.headers_names() {
        if let Some(val) = apple_resp.header(&name) {
            headers.insert(name, plist::Value::String(val.to_owned()));
        }
    }
    let activation_response = {
        let mut buf = Vec::new();
        apple_resp.into_reader().read_to_end(&mut buf)
            .context("read activation response")?;
        buf
    };

    if activation_response.is_empty() {
        bail!("Apple activation server returned an empty response");
    }

    // ── Step 5: install activation record on device ───────────────────────────
    eprintln!("installing activation record on device…");
    client.handle_activation_with_session(&activation_response, headers)
        .context("HandleActivationInfoWithSessionRequest")?;

    println!("device activated successfully");
    Ok(())
}

pub fn deactivate(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut client = ActivationClient::connect(session.lockdown())
        .context("connect activation service")?;
    client.deactivate().context("deactivate")?;
    println!("device deactivated");
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn percent_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 3);
    for &b in data {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => { out.push('%'); out.push(hex_char(b >> 4)); out.push(hex_char(b & 0xf)); }
        }
    }
    out
}

fn hex_char(nibble: u8) -> char {
    if nibble < 10 { (b'0' + nibble) as char } else { (b'A' + nibble - 10) as char }
}
