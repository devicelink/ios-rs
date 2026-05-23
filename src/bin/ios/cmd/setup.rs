use anyhow::{Context, Result};
use ios_rs::lockdown::pairing::{build_pkcs7_signed_data, parse_rsa_key_bytes};
use ios_rs::lockdown::services::mcinstall::{McInstallClient, SKIP_SETUP_KEYS};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

/// Show supervision and Setup-Assistant state.
pub fn status(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut mc = McInstallClient::connect(session.lockdown()).context("connect MCInstall")?;
    mc.flush().context("flush")?;
    let config = mc.get_cloud_config().context("GetCloudConfiguration")?;

    let supervised = config.get("IsSupervised").and_then(|v| v.as_boolean()).unwrap_or(false);
    let complete   = config.get("CloudConfigurationIsComplete").and_then(|v| v.as_boolean());
    let org        = config.get("OrganizationName").and_then(|v| v.as_string());

    println!("supervised:    {supervised}");
    if let Some(name) = org { println!("organization:  {name}"); }
    if let Some(done) = complete { println!("setup complete: {done}"); }
    Ok(())
}

/// Skip the Setup Assistant by sending CloudConfigurationIsComplete.
/// Works on freshly erased or provisioned devices stuck on the Hello/setup screen.
pub fn skip(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut mc = McInstallClient::connect(session.lockdown()).context("connect MCInstall")?;
    mc.flush().context("flush")?;
    mc.hello().context("hello")?;

    let mut cfg = plist::Dictionary::new();
    cfg.insert("AllowPairing".into(), plist::Value::Boolean(true));
    cfg.insert("SkipSetup".into(), plist::Value::Array(
        SKIP_SETUP_KEYS.iter().map(|k| plist::Value::String(k.to_string())).collect()
    ));

    mc.set_cloud_config(cfg).context("SetCloudConfiguration")?;
    println!("Setup Assistant skipped — device will proceed past first-run screens");
    Ok(())
}

/// Skip setup AND enroll as a supervised device.
///
/// Requires the same supervision certificate and private key that were used for
/// supervised pairing.  The cert file may be DER or PEM; the key must be PEM.
pub fn enroll(
    udid:      Option<&str>,
    mode:      ConnectionMode,
    org:       &str,
    cert_path: &str,
    key_path:  &str,
) -> Result<()> {
    let cert_der = load_der_or_pem_cert(cert_path)
        .with_context(|| format!("read supervision cert {cert_path}"))?;
    let key_bytes = std::fs::read(key_path)
        .with_context(|| format!("read supervision key {key_path}"))?;
    let key = parse_rsa_key_bytes(&key_bytes)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let org_magic = random_uuid();

    let mut session = open_session(udid, mode)?;
    let mut mc = McInstallClient::connect(session.lockdown()).context("connect MCInstall")?;
    mc.flush().context("flush")?;
    mc.hello().context("hello")?;

    // Build supervised cloud configuration.
    let mut cfg = plist::Dictionary::new();
    cfg.insert("AllowPairing".into(),     plist::Value::Boolean(true));
    cfg.insert("IsSupervised".into(),     plist::Value::Boolean(true));
    cfg.insert("IsMultiUser".into(),      plist::Value::Boolean(false));
    cfg.insert("OrganizationName".into(), plist::Value::String(org.to_owned()));
    cfg.insert("OrganizationMagic".into(), plist::Value::String(org_magic));
    cfg.insert("SkipSetup".into(), plist::Value::Array(
        SKIP_SETUP_KEYS.iter().map(|k| plist::Value::String(k.to_string())).collect()
    ));
    cfg.insert("SupervisorHostCertificates".into(), plist::Value::Array(
        vec![plist::Value::Data(cert_der.clone())]
    ));

    eprintln!("setting cloud configuration (supervised, org={org:?})…");
    mc.set_cloud_config(cfg).context("SetCloudConfiguration")?;

    // ── Supervised escalation ─────────────────────────────────────────────────
    eprintln!("escalating to supervised mode…");
    let challenge = mc.escalate(&cert_der).context("Escalate")?;

    let signed = build_pkcs7_signed_data(&challenge, &cert_der, &key)
        .map_err(|e| anyhow::anyhow!("sign escalation challenge: {e}"))?;

    mc.escalate_response(&signed).context("EscalateResponse")?;
    mc.proceed_keybag_migration().context("ProceedWithKeybagMigration")?;

    println!("supervised enrollment complete — org={org:?}");
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn load_der_or_pem_cert(path: &str) -> anyhow::Result<Vec<u8>> {
    let raw = std::fs::read(path)?;
    if raw.windows(5).any(|w| w == b"-----") {
        // PEM — strip bag attributes and extract DER
        let pem = std::str::from_utf8(&raw)?;
        let mut b64 = String::new();
        let mut in_block = false;
        for line in pem.lines() {
            if line.starts_with("-----BEGIN") { in_block = true; continue; }
            if line.starts_with("-----END")   { break; }
            if in_block { b64.push_str(line); }
        }
        base64_decode(&b64)
    } else {
        Ok(raw)
    }
}

fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut map = [0xff_u8; 256];
    for (i, &c) in TABLE.iter().enumerate() { map[c as usize] = i as u8; }
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=' && b != b'\n' && b != b'\r').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 { break; }
        let b0 = map[chunk[0] as usize]; let b1 = map[chunk[1] as usize];
        out.push((b0 << 2) | (b1 >> 4));
        if chunk.len() > 2 {
            let b2 = map[chunk[2] as usize];
            out.push((b1 << 4) | (b2 >> 2));
            if chunk.len() > 3 { out.push((b2 << 6) | map[chunk[3] as usize]); }
        }
    }
    Ok(out)
}

fn random_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let n = t.as_nanos();
    format!(
        "{:08X}-{:04X}-4{:03X}-{:04X}-{:012X}",
        (n >> 96) as u32,
        (n >> 80) as u16,
        (n >> 68) as u16 & 0xfff,
        ((n >> 52) as u16 & 0x3fff) | 0x8000,
        n as u64 & 0xffff_ffff_ffff,
    )
}
