/// Plist-over-usbmux framing helpers (server side).
///
/// Wire format: [total_len: u32le][version=1: u32le][type=8: u32le][tag: u32le][plist bytes]
use std::io::{self, Read, Write};
use std::net::TcpStream;

const HEADER_LEN: usize = 16;
const VERSION_PLIST: u32 = 1;
const MSGTYPE_PLIST: u32 = 8;

/// Read exactly `n` bytes from a stream.
fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = s.read(&mut buf[filled..])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "sim: client closed"));
        }
        filled += n;
    }
    Ok(())
}

/// Read one complete usbmux frame. Returns `(tag, plist_bytes)`.
pub fn read_frame(s: &mut TcpStream) -> io::Result<(u32, Vec<u8>)> {
    let mut hdr = [0u8; HEADER_LEN];
    read_exact(s, &mut hdr)?;

    let total = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
    let tag   = u32::from_le_bytes(hdr[12..16].try_into().unwrap());

    let payload_len = total.saturating_sub(HEADER_LEN);
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        read_exact(s, &mut payload)?;
    }
    Ok((tag, payload))
}

/// Write one complete usbmux plist frame.
pub fn write_frame(s: &mut TcpStream, tag: u32, plist: &[u8]) -> io::Result<()> {
    let total = (HEADER_LEN + plist.len()) as u32;
    let mut hdr = [0u8; HEADER_LEN];
    hdr[0..4].copy_from_slice(&total.to_le_bytes());
    hdr[4..8].copy_from_slice(&VERSION_PLIST.to_le_bytes());
    hdr[8..12].copy_from_slice(&MSGTYPE_PLIST.to_le_bytes());
    hdr[12..16].copy_from_slice(&tag.to_le_bytes());
    s.write_all(&hdr)?;
    s.write_all(plist)?;
    s.flush()
}

/// Parse the `MessageType` field from a received plist payload.
pub fn message_type(plist: &[u8]) -> Option<String> {
    let val: plist::Value = plist::from_bytes(plist).ok()?;
    let dict = val.as_dictionary()?;
    dict.get("MessageType")?.as_string().map(|s| s.to_owned())
}

/// Parse the `DeviceID` + `PortNumber` from a Connect plist.
pub fn parse_connect(plist: &[u8]) -> Option<(u32, u16)> {
    let val: plist::Value = plist::from_bytes(plist).ok()?;
    let dict = val.as_dictionary()?;
    let device_id  = dict.get("DeviceID")?.as_unsigned_integer()? as u32;
    let port_be    = dict.get("PortNumber")?.as_unsigned_integer()? as u16;
    let port       = u16::from_be(port_be); // wire uses network (big-endian) byte order
    Some((device_id, port))
}

/// Build a `Result` plist response.
pub fn result_plist(code: u32) -> Vec<u8> {
    let mut d = plist::Dictionary::new();
    d.insert("MessageType".into(), plist::Value::String("Result".into()));
    d.insert("Number".into(), plist::Value::Integer(code.into()));
    encode_plist(&plist::Value::Dictionary(d))
}

/// Build a `DeviceList` plist response from a slice of device dictionaries.
pub fn device_list_plist(device_entries: &[plist::Value]) -> Vec<u8> {
    let mut d = plist::Dictionary::new();
    d.insert(
        "DeviceList".into(),
        plist::Value::Array(device_entries.to_vec()),
    );
    encode_plist(&plist::Value::Dictionary(d))
}

/// Build an `Attached` event plist for a single device.
pub fn attached_plist(device_id: u32, serial: &str, connection_type: &str, product_id: u16) -> Vec<u8> {
    let mut props = plist::Dictionary::new();
    props.insert("SerialNumber".into(),   plist::Value::String(serial.into()));
    props.insert("ConnectionType".into(), plist::Value::String(connection_type.into()));
    props.insert("ProductID".into(),      plist::Value::Integer((product_id as i64).into()));
    props.insert("LocationID".into(),     plist::Value::Integer(0.into()));

    let mut d = plist::Dictionary::new();
    d.insert("DeviceID".into(),    plist::Value::Integer((device_id as i64).into()));
    d.insert("MessageType".into(), plist::Value::String("Attached".into()));
    d.insert("Properties".into(),  plist::Value::Dictionary(props));
    encode_plist(&plist::Value::Dictionary(d))
}

/// Build a `Detached` event plist.
#[allow(dead_code)]
pub fn detached_plist(device_id: u32) -> Vec<u8> {
    let mut d = plist::Dictionary::new();
    d.insert("DeviceID".into(),    plist::Value::Integer((device_id as i64).into()));
    d.insert("MessageType".into(), plist::Value::String("Detached".into()));
    encode_plist(&plist::Value::Dictionary(d))
}

fn encode_plist(val: &plist::Value) -> Vec<u8> {
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, val).expect("plist encode");
    buf
}
