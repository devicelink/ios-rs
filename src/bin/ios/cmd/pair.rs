use anyhow::{Context, Result, anyhow};

use crate::cmd::resolve_device;

pub fn pair(
    udid:                Option<&str>,
    supervision_cert:    Option<&str>,
    supervision_key:     Option<&str>,
    supervision_p12:     Option<&str>,
    supervision_password: Option<&str>,
) -> Result<()> {
    let device = resolve_device(udid)?;

    if let Some(p12_path) = supervision_p12 {
        // P12 path — parse cert + key from the P12 file
        let p12_bytes = std::fs::read(p12_path)
            .with_context(|| format!("read P12 file {p12_path}"))?;
        let password = supervision_password.unwrap_or("");
        let (cert_der, key_der) = extract_from_p12(&p12_bytes, password)?;

        ios_rs::lockdown::pairing::pair_supervised(
            device.device_id, &device.serial, &cert_der, &key_der,
        ).context("supervised pairing (P12)")?;
        println!("supervised pair complete — pair record saved to usbmuxd");

    } else if let (Some(cert_path), Some(key_path)) = (supervision_cert, supervision_key) {
        // Explicit cert + key files
        let cert_bytes = load_der_or_pem_cert(cert_path)
            .with_context(|| format!("read supervision cert {cert_path}"))?;
        let key_bytes = std::fs::read(key_path)
            .with_context(|| format!("read supervision key {key_path}"))?;

        ios_rs::lockdown::pairing::pair_supervised(
            device.device_id, &device.serial, &cert_bytes, &key_bytes,
        ).context("supervised pairing")?;
        println!("supervised pair complete — pair record saved to usbmuxd");

    } else if supervision_cert.is_some() || supervision_key.is_some() {
        anyhow::bail!("--supervision-cert and --supervision-key must both be provided");

    } else {
        // Normal pairing — shows Trust dialog on device
        ios_rs::lockdown::pairing::pair(device.device_id, &device.serial)
            .context("pairing")?;
        println!("paired successfully — pair record saved to usbmuxd");
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

// ── P12 parsing ───────────────────────────────────────────────────────────────

fn extract_from_p12(p12_bytes: &[u8], password: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let pfx = p12::PFX::parse(p12_bytes)
        .map_err(|e| anyhow!("parse P12: {e:?}"))?;

    let certs = pfx.cert_bags(password)
        .map_err(|e| anyhow!("read cert bags (wrong password?): {e:?}"))?;
    let cert_der = certs.into_iter().next()
        .ok_or_else(|| anyhow!("no certificate found in P12"))?;

    let keys = pfx.key_bags(password)
        .map_err(|e| anyhow!("read key bags: {e:?}"))?;
    let key_der = keys.into_iter().next()
        .ok_or_else(|| anyhow!("no private key found in P12"))?;

    Ok((cert_der, key_der))
}

// ── cert file helpers ─────────────────────────────────────────────────────────

/// Load a certificate file — accepts DER (binary) or PEM (text) format.
fn load_der_or_pem_cert(path: &str) -> Result<Vec<u8>> {
    let raw = std::fs::read(path)?;
    if raw.starts_with(b"-----") {
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
