/// Minimal HTTP/2 frame encoder/decoder sufficient for RemoteXPC.
///
/// RemoteXPC uses only DATA, HEADERS, SETTINGS, WINDOW_UPDATE, and PING frames.
/// Flow control is skipped (we send a large WINDOW_UPDATE upfront).
use std::io::{self, Read, Write};

use super::Error;

// ── frame types ──────────────────────────────────────────────────────────────

pub const TYPE_DATA:          u8 = 0x0;
pub const TYPE_HEADERS:       u8 = 0x1;
pub const TYPE_SETTINGS:      u8 = 0x4;
pub const TYPE_PING:          u8 = 0x6;
pub const TYPE_GOAWAY:        u8 = 0x7;
pub const TYPE_WINDOW_UPDATE: u8 = 0x8;

// SETTINGS flags
pub const FLAG_ACK: u8 = 0x1;
// HEADERS flags
pub const FLAG_END_HEADERS: u8 = 0x4;

// SETTINGS IDs
pub const SETTING_MAX_CONCURRENT_STREAMS: u16 = 0x3;
pub const SETTING_INITIAL_WINDOW_SIZE:    u16 = 0x4;

/// HTTP/2 client connection preface (magic + CRLF).
pub const CLIENT_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

// ── frame types ──────────────────────────────────────────────────────────────

#[derive(Debug)]
#[allow(dead_code)] // fields used when RSD/H2 path is fully wired
pub enum Frame {
    Data         { stream_id: u32, payload: Vec<u8> },
    Headers      { stream_id: u32, flags: u8 },
    Settings     { flags: u8, pairs: Vec<(u16, u32)> },
    Ping         { ack: bool, opaque: [u8; 8] },
    GoAway       { last_stream: u32, error: u32 },
    WindowUpdate { stream_id: u32, increment: u32 },
    Unknown      { frame_type: u8, stream_id: u32, payload: Vec<u8> },
}

// ── read ──────────────────────────────────────────────────────────────────────

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }
        filled += n;
    }
    Ok(())
}

pub fn read_frame<R: Read>(r: &mut R) -> Result<Frame, Error> {
    let mut hdr = [0u8; 9];
    read_exact(r, &mut hdr)?;

    let length = (hdr[0] as u32) << 16 | (hdr[1] as u32) << 8 | hdr[2] as u32;
    let frame_type = hdr[3];
    let flags      = hdr[4];
    let stream_id  = u32::from_be_bytes([hdr[5] & 0x7f, hdr[6], hdr[7], hdr[8]]);

    let mut payload = vec![0u8; length as usize];
    if length > 0 {
        read_exact(r, &mut payload)?;
    }

    let frame = match frame_type {
        TYPE_DATA => Frame::Data { stream_id, payload },
        TYPE_HEADERS => Frame::Headers { stream_id, flags },
        TYPE_SETTINGS => {
            let mut pairs = Vec::new();
            let mut i = 0;
            while i + 6 <= payload.len() {
                let id  = u16::from_be_bytes([payload[i],     payload[i + 1]]);
                let val = u32::from_be_bytes([payload[i + 2], payload[i + 3], payload[i + 4], payload[i + 5]]);
                pairs.push((id, val));
                i += 6;
            }
            Frame::Settings { flags, pairs }
        }
        TYPE_PING => {
            let ack = flags & FLAG_ACK != 0;
            let mut opaque = [0u8; 8];
            if payload.len() >= 8 { opaque.copy_from_slice(&payload[..8]); }
            Frame::Ping { ack, opaque }
        }
        TYPE_GOAWAY => {
            let last_stream = u32::from_be_bytes(payload.get(0..4).unwrap_or(&[0;4]).try_into().unwrap()) & 0x7fff_ffff;
            let error       = u32::from_be_bytes(payload.get(4..8).unwrap_or(&[0;4]).try_into().unwrap());
            Frame::GoAway { last_stream, error }
        }
        TYPE_WINDOW_UPDATE => {
            let increment = u32::from_be_bytes(payload[..4].try_into().unwrap_or([0;4])) & 0x7fff_ffff;
            Frame::WindowUpdate { stream_id, increment }
        }
        _ => Frame::Unknown { frame_type, stream_id, payload },
    };

    Ok(frame)
}

// ── write ─────────────────────────────────────────────────────────────────────

fn write_frame_raw<W: Write>(w: &mut W, frame_type: u8, flags: u8, stream_id: u32, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    let mut hdr = [0u8; 9];
    hdr[0] = (len >> 16) as u8;
    hdr[1] = (len >> 8)  as u8;
    hdr[2] =  len        as u8;
    hdr[3] = frame_type;
    hdr[4] = flags;
    hdr[5..9].copy_from_slice(&(stream_id & 0x7fff_ffff).to_be_bytes());
    w.write_all(&hdr)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    Ok(())
}

pub fn write_settings<W: Write>(w: &mut W, pairs: &[(u16, u32)]) -> io::Result<()> {
    let mut payload = Vec::with_capacity(pairs.len() * 6);
    for (id, val) in pairs {
        payload.extend_from_slice(&id.to_be_bytes());
        payload.extend_from_slice(&val.to_be_bytes());
    }
    write_frame_raw(w, TYPE_SETTINGS, 0, 0, &payload)
}

pub fn write_settings_ack<W: Write>(w: &mut W) -> io::Result<()> {
    write_frame_raw(w, TYPE_SETTINGS, FLAG_ACK, 0, &[])
}

pub fn write_window_update<W: Write>(w: &mut W, stream_id: u32, increment: u32) -> io::Result<()> {
    let payload = (increment & 0x7fff_ffff).to_be_bytes();
    write_frame_raw(w, TYPE_WINDOW_UPDATE, 0, stream_id, &payload)
}

pub fn write_headers<W: Write>(w: &mut W, stream_id: u32) -> io::Result<()> {
    // Empty HEADERS with END_HEADERS — RemoteXPC uses no real HTTP headers
    write_frame_raw(w, TYPE_HEADERS, FLAG_END_HEADERS, stream_id, &[])
}

pub fn write_data<W: Write>(w: &mut W, stream_id: u32, payload: &[u8]) -> io::Result<()> {
    write_frame_raw(w, TYPE_DATA, 0, stream_id, payload)
}

pub fn write_ping_ack<W: Write>(w: &mut W, opaque: [u8; 8]) -> io::Result<()> {
    write_frame_raw(w, TYPE_PING, FLAG_ACK, 0, &opaque)
}
