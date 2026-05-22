use std::collections::HashMap;

use super::{
    Message, XpcError, Value,
    PAYLOAD_MAGIC, PAYLOAD_VERSION, WRAPPER_MAGIC,
};

// ── XPC type tags ────────────────────────────────────────────────────────────

const TYPE_NULL: u32       = 0x0000_1000;
const TYPE_BOOL: u32       = 0x0000_2000;
const TYPE_INT64: u32      = 0x0000_3000;
const TYPE_UINT64: u32     = 0x0000_4000;
const TYPE_DOUBLE: u32     = 0x0000_5000;
const TYPE_DATA: u32       = 0x0000_8000;
const TYPE_STRING: u32     = 0x0000_9000;
const TYPE_UUID: u32       = 0x0000_a000;
const TYPE_ARRAY: u32      = 0x0000_e000;
const TYPE_DICTIONARY: u32 = 0x0000_f000;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Round `n` up to the next 4-byte boundary.
fn pad4(n: usize) -> usize { (n + 3) & !3 }

fn read_u32le(buf: &[u8], pos: usize) -> Result<u32, XpcError> {
    buf.get(pos..pos + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or(XpcError::TooShort { need: pos + 4, got: buf.len() })
}

fn read_u64le(buf: &[u8], pos: usize) -> Result<u64, XpcError> {
    buf.get(pos..pos + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
        .ok_or(XpcError::TooShort { need: pos + 8, got: buf.len() })
}

// ── decode ───────────────────────────────────────────────────────────────────

/// Decode an XPC message from `buf`. Returns `(message, bytes_consumed)`.
pub fn decode_message(buf: &[u8]) -> Result<(Message, usize), XpcError> {
    if buf.len() < 24 {
        return Err(XpcError::TooShort { need: 24, got: buf.len() });
    }
    let magic = read_u32le(buf, 0)?;
    if magic != WRAPPER_MAGIC {
        return Err(XpcError::BadMagic(magic));
    }
    let flags    = read_u32le(buf, 4)?;
    let body_len = read_u64le(buf, 8)? as usize;
    let msg_id   = read_u64le(buf, 16)?;

    let total = 24 + body_len;
    if buf.len() < total {
        return Err(XpcError::TooShort { need: total, got: buf.len() });
    }

    let body = if body_len >= 8 {
        let pb = &buf[24..24 + body_len];
        let pmag = read_u32le(pb, 0)?;
        if pmag != PAYLOAD_MAGIC {
            return Err(XpcError::BadMagic(pmag));
        }
        // pb[4..8] = version (skip)
        let (val, _) = decode_value(pb, 8)?;
        Some(val)
    } else {
        None
    };

    Ok((Message { flags, msg_id, body }, total))
}

fn decode_value(buf: &[u8], pos: usize) -> Result<(Value, usize), XpcError> {
    let tag = read_u32le(buf, pos)?;
    let p   = pos + 4; // after the type tag

    match tag {
        TYPE_NULL => Ok((Value::Null, p)),

        TYPE_BOOL => {
            let v = *buf.get(p).ok_or(XpcError::TooShort { need: p + 1, got: buf.len() })? != 0;
            Ok((Value::Bool(v), p + 4)) // bool + 3 pad bytes
        }

        TYPE_INT64 => {
            let n = i64::from_le_bytes(
                buf.get(p..p + 8)
                    .ok_or(XpcError::TooShort { need: p + 8, got: buf.len() })?
                    .try_into().unwrap(),
            );
            Ok((Value::Int64(n), p + 8))
        }

        TYPE_UINT64 => {
            let n = read_u64le(buf, p)?;
            Ok((Value::Uint64(n), p + 8))
        }

        TYPE_DOUBLE => {
            let bits = read_u64le(buf, p)?;
            Ok((Value::Double(f64::from_bits(bits)), p + 8))
        }

        TYPE_DATA => {
            let len = read_u32le(buf, p)? as usize;
            let start = p + 4;
            buf.get(start..start + len)
                .ok_or(XpcError::TooShort { need: start + len, got: buf.len() })?;
            let data = buf[start..start + len].to_vec();
            Ok((Value::Data(data), start + pad4(len)))
        }

        TYPE_STRING => {
            let len = read_u32le(buf, p)? as usize; // includes NUL
            let start = p + 4;
            buf.get(start..start + len)
                .ok_or(XpcError::TooShort { need: start + len, got: buf.len() })?;
            let raw = &buf[start..start + len];
            // strip trailing NUL(s)
            let s = std::str::from_utf8(raw.split(|&b| b == 0).next().unwrap_or(raw))
                .map_err(|_| XpcError::InvalidUtf8)?
                .to_owned();
            Ok((Value::String(s), start + pad4(len)))
        }

        TYPE_UUID => {
            let end = p + 16;
            buf.get(p..end)
                .ok_or(XpcError::TooShort { need: end, got: buf.len() })?;
            let mut u = [0u8; 16];
            u.copy_from_slice(&buf[p..end]);
            Ok((Value::Uuid(u), end))
        }

        TYPE_ARRAY => {
            // wire: total_size(4) | count(4) | items...
            // total_size = 4(count) + items_bytes
            let total_size = read_u32le(buf, p)? as usize;
            let count      = read_u32le(buf, p + 4)? as usize;
            let end        = p + 4 + total_size; // p + total_size_field(4) + total_size
            let mut cur    = p + 8;
            let mut items  = Vec::with_capacity(count);
            for _ in 0..count {
                let (val, next) = decode_value(buf, cur)?;
                items.push(val);
                cur = next;
            }
            Ok((Value::Array(items), end))
        }

        TYPE_DICTIONARY => {
            let total_size = read_u32le(buf, p)? as usize;
            let count      = read_u32le(buf, p + 4)? as usize;
            let end        = p + 4 + total_size;
            let mut cur    = p + 8;
            let mut map    = HashMap::with_capacity(count);
            for _ in 0..count {
                // Key: NUL-terminated string padded to 4 bytes
                let nul = buf[cur..]
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or(XpcError::InvalidUtf8)?;
                let key = std::str::from_utf8(&buf[cur..cur + nul])
                    .map_err(|_| XpcError::InvalidUtf8)?
                    .to_owned();
                cur += pad4(nul + 1);
                let (val, next) = decode_value(buf, cur)?;
                map.insert(key, val);
                cur = next;
            }
            Ok((Value::Dictionary(map), end))
        }

        // 0x7000 = XPC_TYPE_DATE — 8-byte double (seconds since reference epoch).
        // Decode as a Double so dict traversal can continue past it.
        0x0000_7000 => {
            let bits = read_u64le(buf, p)?;
            Ok((Value::Double(f64::from_bits(bits)), p + 8))
        }

        // Unknown fixed-size scalar — skip 8 bytes and return Null so callers
        // can still traverse the surrounding dict/array.
        other => {
            let _ = other;
            Ok((Value::Null, p + 8))
        }
    }
}

// ── encode ───────────────────────────────────────────────────────────────────

/// Encode an XPC message to bytes.
pub fn encode_message(msg: &Message) -> Vec<u8> {
    let payload = msg.body.as_ref().map(|v| {
        let mut p = Vec::new();
        p.extend_from_slice(&PAYLOAD_MAGIC.to_le_bytes());
        p.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
        encode_value(v, &mut p);
        p
    });

    let body_len = payload.as_ref().map_or(0, |p| p.len()) as u64;
    let mut out  = Vec::with_capacity(24 + body_len as usize);
    out.extend_from_slice(&WRAPPER_MAGIC.to_le_bytes());
    out.extend_from_slice(&msg.flags.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&msg.msg_id.to_le_bytes());
    if let Some(p) = payload {
        out.extend_from_slice(&p);
    }
    out
}

fn encode_value(val: &Value, out: &mut Vec<u8>) {
    match val {
        Value::Null => {
            out.extend_from_slice(&TYPE_NULL.to_le_bytes());
        }
        Value::Bool(b) => {
            out.extend_from_slice(&TYPE_BOOL.to_le_bytes());
            out.push(if *b { 1 } else { 0 });
            out.extend_from_slice(&[0u8, 0, 0]); // 3 pad bytes
        }
        Value::Int64(n) => {
            out.extend_from_slice(&TYPE_INT64.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Uint64(n) => {
            out.extend_from_slice(&TYPE_UINT64.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Double(d) => {
            out.extend_from_slice(&TYPE_DOUBLE.to_le_bytes());
            out.extend_from_slice(&d.to_bits().to_le_bytes());
        }
        Value::Data(bytes) => {
            let pad = pad4(bytes.len()) - bytes.len();
            out.extend_from_slice(&TYPE_DATA.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        Value::String(s) => {
            let len = s.len() + 1; // includes NUL
            let pad = pad4(len) - len;
            out.extend_from_slice(&TYPE_STRING.to_le_bytes());
            out.extend_from_slice(&(len as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
            out.push(0); // NUL
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        Value::Uuid(u) => {
            out.extend_from_slice(&TYPE_UUID.to_le_bytes());
            out.extend_from_slice(u);
        }
        Value::Array(items) => {
            // Encode items first to compute sizes
            let mut items_buf = Vec::new();
            for item in items {
                encode_value(item, &mut items_buf);
            }
            // wire: TYPE | total_size(4) | count(4) | items
            // total_size = 4(count) + items_buf.len()
            let total_size = 4u32 + items_buf.len() as u32;
            out.extend_from_slice(&TYPE_ARRAY.to_le_bytes());
            out.extend_from_slice(&total_size.to_le_bytes());
            out.extend_from_slice(&(items.len() as u32).to_le_bytes());
            out.extend_from_slice(&items_buf);
        }
        Value::Dictionary(map) => {
            // Encode key-value pairs first
            let mut kv_buf = Vec::new();
            for (key, val) in map {
                encode_dict_key(key, &mut kv_buf);
                encode_value(val, &mut kv_buf);
            }
            // wire: TYPE | total_size(4) | count(4) | kv_pairs
            // total_size = 4(count) + kv_buf.len()
            let total_size = 4u32 + kv_buf.len() as u32;
            out.extend_from_slice(&TYPE_DICTIONARY.to_le_bytes());
            out.extend_from_slice(&total_size.to_le_bytes());
            out.extend_from_slice(&(map.len() as u32).to_le_bytes());
            out.extend_from_slice(&kv_buf);
        }
    }
}

fn encode_dict_key(key: &str, out: &mut Vec<u8>) {
    let len = key.len() + 1; // includes NUL
    let pad = pad4(len) - len;
    out.extend_from_slice(key.as_bytes());
    out.push(0);
    out.extend(std::iter::repeat_n(0u8, pad));
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xpc::flags;

    #[test]
    fn roundtrip_string() {
        let msg = Message::with_body(1, Value::String("hello".into()));
        let enc = encode_message(&msg);
        let (dec, n) = decode_message(&enc).unwrap();
        assert_eq!(n, enc.len());
        assert_eq!(dec.msg_id, 1);
        assert_eq!(dec.body.unwrap().as_str().unwrap(), "hello");
    }

    #[test]
    fn roundtrip_dict() {
        let mut d = HashMap::new();
        d.insert("answer".into(), Value::Uint64(42));
        d.insert("name".into(), Value::String("test".into()));
        let msg = Message::with_body(2, Value::Dictionary(d));
        let enc = encode_message(&msg);
        let (dec, _) = decode_message(&enc).unwrap();
        let map = dec.body.unwrap();
        let m = map.as_dict().unwrap();
        assert_eq!(m["answer"].as_u64().unwrap(), 42);
        assert_eq!(m["name"].as_str().unwrap(), "test");
    }

    #[test]
    fn roundtrip_array() {
        let arr = Value::Array(vec![
            Value::Uint64(1),
            Value::String("two".into()),
            Value::Bool(false),
        ]);
        let msg = Message::with_body(3, arr);
        let enc = encode_message(&msg);
        let (dec, _) = decode_message(&enc).unwrap();
        let a = dec.body.unwrap();
        let items = a.as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_u64().unwrap(), 1);
        assert_eq!(items[1].as_str().unwrap(), "two");
        assert_eq!(items[2].as_bool().unwrap(), false);
    }

    #[test]
    fn roundtrip_bool() {
        for b in [true, false] {
            let msg = Message::with_body(4, Value::Bool(b));
            let (dec, _) = decode_message(&encode_message(&msg)).unwrap();
            assert_eq!(dec.body.unwrap().as_bool().unwrap(), b);
        }
    }

    #[test]
    fn no_body_message() {
        let msg = Message::init(flags::ALWAYS_SET);
        let enc = encode_message(&msg);
        assert_eq!(enc.len(), 24);
        let (dec, n) = decode_message(&enc).unwrap();
        assert_eq!(n, 24);
        assert!(dec.body.is_none());
    }

    #[test]
    fn nested_dict() {
        let mut inner = HashMap::new();
        inner.insert("port".into(), Value::Uint64(58783));
        let mut outer = HashMap::new();
        outer.insert("com.apple.coredevice.appservice".into(), Value::Dictionary(inner));
        let msg = Message::with_body(5, Value::Dictionary(outer));
        let enc = encode_message(&msg);
        let (dec, _) = decode_message(&enc).unwrap();
        let top = dec.body.unwrap();
        let svc = &top.as_dict().unwrap()["com.apple.coredevice.appservice"];
        assert_eq!(svc.as_dict().unwrap()["port"].as_u64().unwrap(), 58783);
    }
}
