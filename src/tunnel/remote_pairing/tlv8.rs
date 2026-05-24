/// TLV8 encoder/decoder used in the RemotePairing handshake.
///
/// Wire format: [type: u8][length: u8][value: length bytes] ...
/// Values > 255 bytes are split into multiple entries of the same type.
use std::collections::HashMap;

// ── well-known type codes ────────────────────────────────────────────────────

pub const METHOD: u8 = 0x00;
pub const IDENTIFIER: u8 = 0x01;
pub const SALT: u8 = 0x02;
pub const PUBLIC_KEY: u8 = 0x03;
pub const PROOF: u8 = 0x04;
pub const ENCRYPTED_DATA: u8 = 0x05;
pub const STATE: u8 = 0x06;
pub const ERROR: u8 = 0x07;
pub const SIGNATURE: u8 = 0x0a;
#[allow(dead_code)]
pub const INFO: u8 = 0x11;

// ── encode ────────────────────────────────────────────────────────────────────

pub fn encode(entries: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(typ, val) in entries {
        // Fragment into ≤255 byte chunks
        if val.is_empty() {
            out.push(typ);
            out.push(0);
        } else {
            for chunk in val.chunks(255) {
                out.push(typ);
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
        }
    }
    out
}

// ── decode ────────────────────────────────────────────────────────────────────

/// Decode TLV8 bytes into a map, reassembling fragmented values.
pub fn decode(data: &[u8]) -> HashMap<u8, Vec<u8>> {
    let mut map: HashMap<u8, Vec<u8>> = HashMap::new();
    let mut i = 0;
    while i + 1 < data.len() {
        let typ = data[i];
        let len = data[i + 1] as usize;
        i += 2;
        let end = (i + len).min(data.len());
        map.entry(typ).or_default().extend_from_slice(&data[i..end]);
        i = end;
    }
    map
}

/// Get a single-byte value from a decoded map.
pub fn get_u8(map: &HashMap<u8, Vec<u8>>, key: u8) -> Option<u8> {
    map.get(&key).and_then(|v| v.first().copied())
}
