use anyhow::{Context, Result};

use crate::cmd::resolve_device;

pub fn export(udid: Option<&str>, path: Option<&str>) -> Result<()> {
    let device = resolve_device(udid)?;
    let mut conn = ios_rs::usbmux::Connection::open()?;
    let raw = conn.read_pair_record(&device.serial)
        .context("read pair record from usbmuxd")?;
    let out_path = path.map(|s| s.to_owned())
        .unwrap_or_else(|| format!("{}.plist", device.serial));
    std::fs::write(&out_path, &raw)
        .with_context(|| format!("write {out_path}"))?;
    println!("pair record saved to {out_path}");
    Ok(())
}

pub fn import(udid: Option<&str>, path: &str) -> Result<()> {
    let device = resolve_device(udid)?;
    let raw = std::fs::read(path)
        .with_context(|| format!("read {path}"))?;
    // Validate it's a valid plist before saving
    let _: plist::Value = plist::from_bytes(&raw)
        .with_context(|| format!("{path} is not a valid plist"))?;
    let mut conn = ios_rs::usbmux::Connection::open()?;
    conn.save_pair_record(&device.serial, raw)
        .context("save pair record to usbmuxd")?;
    println!("pair record imported from {path}");
    Ok(())
}

pub fn pair(
    udid:             Option<&str>,
    supervision_cert: Option<&str>,
    supervision_key:  Option<&str>,
) -> Result<()> {
    let device = resolve_device(udid)?;

    match (supervision_cert, supervision_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_bytes = load_der_or_pem_cert(cert_path)
                .with_context(|| format!("read supervision cert {cert_path}"))?;
            let key_bytes = std::fs::read(key_path)
                .with_context(|| format!("read supervision key {key_path}"))?;
            ios_rs::lockdown::pairing::pair_supervised(
                device.device_id, &device.serial, &cert_bytes, &key_bytes,
            ).context("supervised pairing")?;
            println!("supervised pair complete — pair record saved to usbmuxd");
        }
        (None, None) => {
            ios_rs::lockdown::pairing::pair(device.device_id, &device.serial)
                .context("pairing")?;
            println!("paired successfully — pair record saved to usbmuxd");
        }
        _ => anyhow::bail!("--supervision-cert and --supervision-key must both be provided"),
    }
    Ok(())
}

pub fn unpair(udid: Option<&str>) -> Result<()> {
    let device = resolve_device(udid)?;
    ios_rs::lockdown::pairing::unpair(device.device_id, &device.serial)
        .context("unpair")?;
    println!("unpaired — pair record deleted");
    Ok(())
}

// ── cert file helpers ─────────────────────────────────────────────────────────

/// Load a cert — accepts DER (binary) or PEM (text, strips bag attributes).
fn load_der_or_pem_cert(path: &str) -> Result<Vec<u8>> {
    let raw = std::fs::read(path)?;
    if raw.starts_with(b"-----") || raw.windows(5).any(|w| w == b"-----") {
        // Find PEM block (skip openssl bag attributes header)
        let pem = std::str::from_utf8(&raw)?;
        let mut b64 = String::new();
        let mut in_block = false;
        for line in pem.lines() {
            if line.starts_with("-----BEGIN") { in_block = true; continue; }
            if line.starts_with("-----END")   { break; }
            if in_block { b64.push_str(line); }
        }
        Ok(base64_decode(&b64)?)
    } else {
        Ok(raw)
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
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
