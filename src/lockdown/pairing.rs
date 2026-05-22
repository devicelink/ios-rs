//! iOS device pairing via lockdownd.
//!
//! Protocol (go-ios ios/pair.go):
//!   1. ReadBUID from usbmuxd
//!   2. Connect to lockdownd (no TLS)
//!   3. GetValue DevicePublicKey (PEM-encoded PKCS#1 RSA public key)
//!   4. GetValue WiFiAddress
//!   5. Generate root / host RSA-2048 key pairs + 3 x.509 certs
//!   6. Send Pair request to lockdownd
//!   7. On success, save pair record via usbmuxd SavePairRecord
//!
//! Certificate format (exactly as go-ios / iOS expects):
//!   - SHA1WithRSA signature algorithm
//!   - Serial 0, empty subject/issuer
//!   - 10-year validity
//!   - Extension OID 2.5.29.14 (SKI): 20-byte raw SHA1 of inner public key bit-string
//!   - Root: BasicConstraints critical, cA=TRUE
//!   - Host/Device: KeyUsage critical, digitalSignature + keyEncipherment
use std::time::{SystemTime, UNIX_EPOCH};

use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::EncodePublicKey;
use rsa::signature::SignatureEncoding;
use rsa::signature::Signer as _;
use rsa::RsaPrivateKey;
use sha1::{Digest, Sha1};

use super::Error;
use crate::usbmux::Connection as MuxConn;

// ── public entry points ───────────────────────────────────────────────────────

/// Attempt to pair with the device.  Returns `Ok(())` when the pair record has
/// been saved.  Returns an error with a user-facing message when the device is
/// waiting for the trust dialog to be accepted — callers should retry.
pub fn pair(device_id: u32, udid: &str) -> Result<(), Error> {
    let mut usbmux = MuxConn::open()?;
    let buid = usbmux.read_buid().map_err(|e| err(format!("read BUID: {e}")))?;

    // Plain (pre-TLS) lockdown session to read device values and send Pair.
    let mut session = super::LockdownSession::connect(device_id)?;

    let pub_key_pem = match session.get_value(None, "DevicePublicKey")? {
        plist::Value::Data(b) => b,
        other => return Err(err(format!("DevicePublicKey unexpected type: {other:?}"))),
    };
    let wifi_mac = match session.get_value(None, "WiFiAddress")? {
        plist::Value::String(s) => s,
        other => return Err(err(format!("WiFiAddress unexpected type: {other:?}"))),
    };

    let (root_cert, host_cert, device_cert, root_key, host_key) =
        create_pairing_certs(&pub_key_pem)?;

    let host_id = random_upper_uuid();

    // Build pair-record dict for the Pair request (sent to lockdownd).
    let mut pair_record = plist::Dictionary::new();
    pair_record.insert("DeviceCertificate".into(), plist::Value::Data(device_cert.clone()));
    pair_record.insert("HostCertificate".into(),   plist::Value::Data(host_cert.clone()));
    pair_record.insert("RootCertificate".into(),   plist::Value::Data(root_cert.clone()));
    pair_record.insert("SystemBUID".into(),        plist::Value::String(buid.clone()));
    pair_record.insert("HostID".into(),            plist::Value::String(host_id.clone()));

    let mut opts = plist::Dictionary::new();
    opts.insert("ExtendedPairingErrors".into(), plist::Value::Boolean(true));

    let mut req = plist::Dictionary::new();
    req.insert("Label".into(),           plist::Value::String("ios-rs".into()));
    req.insert("ProtocolVersion".into(), plist::Value::String("2".into()));
    req.insert("Request".into(),         plist::Value::String("Pair".into()));
    req.insert("PairRecord".into(),      plist::Value::Dictionary(pair_record));
    req.insert("PairingOptions".into(),  plist::Value::Dictionary(opts));

    session.send_raw(&plist::Value::Dictionary(req))?;
    let resp = session.recv_raw()?;

    let resp_dict = resp.as_dictionary()
        .ok_or_else(|| err("Pair response not a dictionary".into()))?;

    if let Some(plist::Value::String(e)) = resp_dict.get("Error") {
        if e == "PairingDialogResponsePending" {
            return Err(err(
                "trust dialog open — accept 'Trust This Computer' on the device, then run 'ios pair' again".into()
            ));
        }
        return Err(err(format!("lockdownd error: {e}")));
    }

    let escrow_bag = match resp_dict.get("EscrowBag") {
        Some(plist::Value::Data(b)) => b.clone(),
        _ => return Err(err("no EscrowBag in pair response".into())),
    };

    // Build the full save-record plist (contains private keys + EscrowBag).
    let mut save_rec = plist::Dictionary::new();
    save_rec.insert("DeviceCertificate".into(), plist::Value::Data(device_cert));
    save_rec.insert("HostPrivateKey".into(),    plist::Value::Data(host_key));
    save_rec.insert("HostCertificate".into(),   plist::Value::Data(host_cert));
    save_rec.insert("RootPrivateKey".into(),    plist::Value::Data(root_key));
    save_rec.insert("RootCertificate".into(),   plist::Value::Data(root_cert));
    save_rec.insert("EscrowBag".into(),         plist::Value::Data(escrow_bag));
    save_rec.insert("WiFiMACAddress".into(),    plist::Value::String(wifi_mac));
    save_rec.insert("HostID".into(),            plist::Value::String(host_id));
    save_rec.insert("SystemBUID".into(),        plist::Value::String(buid));

    let mut raw = Vec::new();
    plist::to_writer_xml(&mut raw, &plist::Value::Dictionary(save_rec))?;

    let mut usbmux2 = MuxConn::open()?;
    usbmux2.save_pair_record(udid, raw)
        .map_err(|e| err(format!("save pair record: {e}")))?;

    Ok(())
}

/// Unpair: remove the pair record from usbmuxd and send Unpair to lockdownd.
pub fn unpair(device_id: u32, udid: &str) -> Result<(), Error> {
    // First tell lockdownd (best-effort — may fail if already unpaired)
    if let Ok(mut session) = super::LockdownSession::connect(device_id) {
        let mut req = plist::Dictionary::new();
        req.insert("Label".into(),   plist::Value::String("ios-rs".into()));
        req.insert("Request".into(), plist::Value::String("Unpair".into()));
        // Fill pair record fields if we can read them
        if let Ok(raw) = {
            let mut mc = MuxConn::open()?;
            mc.read_pair_record(udid)
        } {
            if let Ok(pr) = super::PairRecord::from_plist_bytes(&raw) {
                let mut pr_dict = plist::Dictionary::new();
                pr_dict.insert("HostID".into(),    plist::Value::String(pr.host_id.clone()));
                pr_dict.insert("SystemBUID".into(), plist::Value::String(pr.system_buid.clone()));
                req.insert("PairRecord".into(), plist::Value::Dictionary(pr_dict));
            }
        }
        let _ = session.send_raw(&plist::Value::Dictionary(req));
    }

    // Delete the record from usbmuxd
    let mut mc = MuxConn::open()?;
    mc.delete_pair_record(udid)
        .map_err(|e| err(format!("delete pair record: {e}")))?;
    Ok(())
}

// ── certificate generation ────────────────────────────────────────────────────

/// Returns (root_cert_pem, host_cert_pem, device_cert_pem, root_key_pem, host_key_pem)
/// All as PEM-encoded bytes.
fn create_pairing_certs(
    device_pub_pem: &[u8],
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>), Error> {
    let mut rng = rand_core_os();

    // Root key pair
    let root_key = RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|e| err(format!("root keygen: {e}")))?;

    // Host key pair
    let host_key = RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|e| err(format!("host keygen: {e}")))?;

    // Device public key from PEM (PKCS#1 format)
    let device_pub = rsa::RsaPublicKey::from_pkcs1_pem(
        std::str::from_utf8(device_pub_pem).map_err(|_| err("device key not utf8".into()))?,
    ).map_err(|e| err(format!("parse device public key: {e}")))?;

    let now  = unix_now();
    let then = now + 10 * 365 * 24 * 3600;

    // Root cert (self-signed, isCA=true)
    let root_cert = build_cert(
        &root_key.to_public_key(),
        CertKind::Root,
        &root_key,
        now,
        then,
    )?;

    // Host cert (signed by root, isCA=false)
    let host_cert = build_cert(
        &host_key.to_public_key(),
        CertKind::Leaf,
        &root_key,
        now,
        then,
    )?;

    // Device cert (device public key signed by root, isCA=false)
    let device_cert = build_cert(
        &device_pub,
        CertKind::Leaf,
        &root_key,
        now,
        then,
    )?;

    Ok((
        pem_encode("CERTIFICATE", &root_cert),
        pem_encode("CERTIFICATE", &host_cert),
        pem_encode("CERTIFICATE", &device_cert),
        root_key.to_pkcs1_pem(Default::default())
            .map_err(|e| err(format!("root key pem: {e}")))?
            .as_bytes().to_vec(),
        host_key.to_pkcs1_pem(Default::default())
            .map_err(|e| err(format!("host key pem: {e}")))?
            .as_bytes().to_vec(),
    ))
}

#[derive(Copy, Clone)]
enum CertKind { Root, Leaf }

/// Build a DER-encoded X.509 v3 certificate with SHA1WithRSA, signed by `signing_key`.
/// Subject and issuer are both empty sequences (as iOS pairing requires).
fn build_cert(
    pub_key:     &rsa::RsaPublicKey,
    kind:        CertKind,
    signing_key: &RsaPrivateKey,
    not_before:  u64,
    not_after:   u64,
) -> Result<Vec<u8>, Error> {
    // SubjectPublicKeyInfo DER for the subject key
    let spki_der = pub_key.to_public_key_der()
        .map_err(|e| err(format!("encode spki: {e}")))?;

    // SHA1 of the raw inner bit-string (same as go-ios computeSKIKey)
    let ski = ski_hash(spki_der.as_bytes());

    // Assemble TBS (To-Be-Signed) certificate
    let tbs = der_seq(&[
        // version [0] EXPLICIT INTEGER (2) → v3
        &der_ctx(0, &der_int(&[2])),
        // serialNumber INTEGER 0
        &der_int(&[0]),
        // signature AlgorithmIdentifier
        &sha1_with_rsa_oid(),
        // issuer empty SEQUENCE
        &der_seq(&[]),
        // validity
        &der_seq(&[
            &der_utc_time(not_before),
            &der_utc_time(not_after),
        ]),
        // subject empty SEQUENCE
        &der_seq(&[]),
        // subjectPublicKeyInfo (already DER)
        spki_der.as_bytes(),
        // extensions [3] EXPLICIT
        &der_ctx(3, &{
            let exts = build_extensions(&ski, kind);
            let refs: Vec<&[u8]> = exts.iter().map(|v| v.as_slice()).collect();
            der_seq(&refs)
        }),
    ]);

    // Sign TBS with SHA1WithRSA
    let signing = rsa::pkcs1v15::SigningKey::<sha1::Sha1>::new(signing_key.clone());
    let sig = signing.sign(&tbs);
    let sig_bytes = sig.to_bytes();

    // Assemble Certificate = SEQUENCE { tbs, alg, BIT STRING sig }
    let mut bit_string_content = vec![0u8]; // leading 0 = no unused bits
    bit_string_content.extend_from_slice(&sig_bytes);

    let cert = der_seq(&[
        &tbs,
        &sha1_with_rsa_oid(),
        &der_tag(0x03, &bit_string_content),
    ]);
    Ok(cert)
}

fn build_extensions(ski: &[u8; 20], kind: CertKind) -> Vec<Vec<u8>> {
    let mut exts = vec![
        // SubjectKeyIdentifier (OID 2.5.29.14): raw 20-byte hash (no OCTET STRING wrapper,
        // matching go-ios behaviour which iOS accepts)
        der_seq(&[
            &der_oid(&[2, 5, 29, 14]),
            &der_tag(0x04, ski),   // OCTET STRING wrapping the raw hash
        ]),
    ];

    match kind {
        CertKind::Root => {
            // BasicConstraints critical, cA = TRUE
            // value = OCTET STRING { SEQUENCE { BOOLEAN TRUE } }
            let bc_inner = der_seq(&[&der_bool(true)]);
            exts.push(der_seq(&[
                &der_oid(&[2, 5, 29, 19]),
                &der_bool(true),  // critical
                &der_tag(0x04, &bc_inner),
            ]));
        }
        CertKind::Leaf => {
            // KeyUsage critical: digitalSignature (bit 0) + keyEncipherment (bit 2)
            // Encoded as BIT STRING: 0x05 (unused=5), 0xa0 (bits 0+2 set in high byte)
            let ku_inner = der_seq(&[&der_tag(0x03, &[0x05, 0xa0])]);
            exts.push(der_seq(&[
                &der_oid(&[2, 5, 29, 15]),
                &der_bool(true),  // critical
                &der_tag(0x04, &ku_inner),
            ]));
        }
    }
    exts
}

// ── minimal DER encoding ──────────────────────────────────────────────────────

fn der_tag(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
    out.extend_from_slice(content);
    out
}

fn der_seq(items: &[&[u8]]) -> Vec<u8> {
    let body: Vec<u8> = items.iter().flat_map(|s| s.iter().copied()).collect();
    der_tag(0x30, &body)
}

fn der_ctx(n: u8, content: &[u8]) -> Vec<u8> { der_tag(0xa0 | n, content) }

fn der_int(v: &[u8]) -> Vec<u8> {
    // Prepend 0x00 if high bit set (to keep it positive)
    let mut body = if v[0] & 0x80 != 0 { vec![0x00] } else { vec![] };
    body.extend_from_slice(v);
    der_tag(0x02, &body)
}

fn der_bool(b: bool) -> Vec<u8> { der_tag(0x01, &[if b { 0xff } else { 0x00 }]) }

fn der_oid(arcs: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    // First two arcs combined: 40 * arc[0] + arc[1]
    body.push((40 * arcs[0] + arcs[1]) as u8);
    for &arc in &arcs[2..] {
        // Base-128 big-endian encoding
        let mut buf = [0u8; 5];
        let mut i = 4;
        let mut a = arc;
        buf[i] = (a & 0x7f) as u8;
        a >>= 7;
        while a > 0 {
            i -= 1;
            buf[i] = ((a & 0x7f) | 0x80) as u8;
            a >>= 7;
        }
        body.extend_from_slice(&buf[i..]);
    }
    der_tag(0x06, &body)
}

fn der_utc_time(unix: u64) -> Vec<u8> {
    // UTCTime: YYMMDDHHMMSSZ
    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    // Days since 1970-01-01
    let days = unix / secs_per_day;
    let rem  = unix % secs_per_day;
    let h = rem / secs_per_hour;
    let m = (rem % secs_per_hour) / 60;
    let s = rem % 60;
    // Year/month/day from days — simple but correct for 10-year range
    let (yr, mo, da) = days_to_ymd(days);
    let yr2 = yr % 100;
    let t = format!("{yr2:02}{mo:02}{da:02}{h:02}{m:02}{s:02}Z");
    der_tag(0x17, t.as_bytes())
}

fn sha1_with_rsa_oid() -> Vec<u8> {
    // OID 1.2.840.113549.1.1.5 = sha1WithRSAEncryption, followed by NULL
    der_seq(&[
        &der_oid(&[1, 2, 840, 113549, 1, 1, 5]),
        &[0x05, 0x00], // NULL
    ])
}

fn ski_hash(spki_der: &[u8]) -> [u8; 20] {
    // Replicate go-ios computeSKIKey: SHA1 of the raw inner bit-string bytes
    // inside SubjectPublicKeyInfo. Parse to find the BIT STRING.
    let inner = extract_spki_bits(spki_der).unwrap_or(spki_der);
    let mut h = Sha1::new();
    h.update(inner);
    h.finalize().into()
}

/// Extract the raw byte content of the BIT STRING inside SubjectPublicKeyInfo.
fn extract_spki_bits(spki: &[u8]) -> Option<&[u8]> {
    // SEQUENCE { AlgorithmIdentifier, BIT STRING }
    // Skip outer SEQUENCE header
    let (_, inner) = peel_tag(spki, 0x30)?;
    // Skip AlgorithmIdentifier
    let (_, rest) = peel_tlv(inner)?;
    // BIT STRING: tag 0x03, first content byte = unused bits count
    let (_, bs_content) = peel_tag(rest, 0x03)?;
    Some(&bs_content[1..]) // skip "unused bits" byte
}

fn peel_tag<'a>(buf: &'a [u8], expected: u8) -> Option<(&'a [u8], &'a [u8])> {
    if buf.first() != Some(&expected) { return None; }
    let (content, rest) = peel_length(&buf[1..])?;
    Some((content, rest))
}

fn peel_tlv(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    if buf.is_empty() { return None; }
    let (content, rest) = peel_length(&buf[1..])?;
    Some((content, rest))
}

fn peel_length(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = *buf.first()?;
    let (len, start) = if first < 0x80 {
        (first as usize, 1)
    } else {
        let n = (first & 0x7f) as usize;
        if buf.len() < 1 + n { return None; }
        let mut len = 0usize;
        for &b in &buf[1..1 + n] { len = (len << 8) | b as usize; }
        (len, 1 + n)
    };
    let end = start + len;
    if buf.len() < end { return None; }
    Some((&buf[start..end], &buf[end..]))
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Gregorian calendar from 1970-01-01
    let mut y = 1970u64;
    loop {
        let leap = is_leap(y);
        let dy = if leap { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        y += 1;
    }
    let leap = is_leap(y);
    let months = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 1u64;
    for &dm in &months {
        if days < dm { break; }
        days -= dm;
        m += 1;
    }
    (y, m, days + 1)
}

fn is_leap(y: u64) -> bool { y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) }

fn pem_encode(label: &str, der: &[u8]) -> Vec<u8> {
    let b64 = base64_encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out.into_bytes()
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= input.len() {
        let b = (input[i] as u32) << 16 | (input[i+1] as u32) << 8 | input[i+2] as u32;
        out.push(TABLE[((b >> 18) & 63) as usize] as char);
        out.push(TABLE[((b >> 12) & 63) as usize] as char);
        out.push(TABLE[((b >>  6) & 63) as usize] as char);
        out.push(TABLE[((b      ) & 63) as usize] as char);
        i += 3;
    }
    match input.len() - i {
        1 => {
            let b = (input[i] as u32) << 16;
            out.push(TABLE[((b >> 18) & 63) as usize] as char);
            out.push(TABLE[((b >> 12) & 63) as usize] as char);
            out.push_str("==");
        }
        2 => {
            let b = (input[i] as u32) << 16 | (input[i+1] as u32) << 8;
            out.push(TABLE[((b >> 18) & 63) as usize] as char);
            out.push(TABLE[((b >> 12) & 63) as usize] as char);
            out.push(TABLE[((b >>  6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn random_upper_uuid() -> String {
    let t = unix_now() as u128 * 1_000_000
        + SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_micros() as u128;
    let b = t.to_le_bytes();
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[0],b[1],b[2],b[3], b[4],b[5],
        (b[6]&0x0f)|0x40, b[7],
        (b[8]&0x3f)|0x80, b[9],
        b[10],b[11],b[12],b[13],b[14],b[15]
    )
}

fn rand_core_os() -> rand::rngs::OsRng { rand::rngs::OsRng }

fn err(msg: String) -> Error { Error::Lockdown(msg) }
