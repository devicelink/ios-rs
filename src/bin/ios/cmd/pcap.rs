//! Packet capture via `com.apple.pcapd.shim.remote`.
//!
//! The service delivers packets as 4-byte-length-prefixed plist messages,
//! each containing a binary blob with a proprietary header followed by the
//! raw frame bytes.  We reassemble these into a standard pcap file.
//!
//! Binary record layout (all multibyte fields big-endian unless noted):
//!   [0]   u32  header_length      (baseline 95 bytes; skip future extensions)
//!   [4]   u8   header_version
//!   [5]   u32  packet_length      (raw frame byte count)
//!   [9]   u8   interface_type     (SNMP ifType)
//!   [10]  u16  unit
//!   [12]  u8   io                 (0=in, 1=out)
//!   [13]  u32  protocol_family    (AF_INET=2, AF_INET6=30)
//!   [17]  u32  frame_pre_length   (0 for cellular — add fake Ethernet header)
//!   [21]  u32  frame_post_length
//!   [25]  [16] interface_name     (NUL-padded, e.g. "en0")
//!   [41]  i32  pid                (little-endian)
//!   [45]  [17] comm               (process name, NUL-padded)
//!   [62]  u32  svc                (little-endian)
//!   [66]  i32  epid               (little-endian)
//!   [70]  [17] ecomm
//!   [87]  u32  ts_sec
//!   [91]  u32  ts_usec
//!   [header_length..] frame bytes (packet_length bytes)
use std::io::{Read, Write};

use anyhow::{Context, Result};
use ios_rs::tunnel::ConnectionMode;
use plist::Value;

use crate::cmd::open_session;

const SHIM: &str = "com.apple.pcapd.shim.remote";

// Fake Ethernet header prepended for non-Ethernet frames (e.g. cellular).
const FAKE_ETH: [u8; 14] = [
    0xbe, 0xef, 0xbe, 0xef, 0xbe, 0xef, // dst MAC
    0xbe, 0xef, 0xbe, 0xef, 0xbe, 0xef, // src MAC
    0x08, 0x00,                           // EtherType: IPv4
];

pub fn run(udid: Option<&str>, mode: ConnectionMode, output: Option<&str>) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut stream  = session.connect_rsd_shim(SHIM).context("connect pcap shim")?;

    let mut out: Box<dyn Write> = match output {
        None    => Box::new(std::io::stdout()),
        Some(p) => Box::new(std::fs::File::create(p)
            .with_context(|| format!("create {p}"))?),
    };

    // Send activation plist to start the capture stream.
    {
        let mut start = plist::Dictionary::new();
        start.insert("StartCapture".into(), Value::Integer(1.into()));
        start.insert("Interface".into(), Value::String("any".into()));
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, &Value::Dictionary(start))?;
        let len = body.len() as u32;
        stream.write_all(&len.to_be_bytes()).context("send StartCapture")?;
        stream.write_all(&body).context("send StartCapture body")?;
        stream.flush()?;
    }

    // Global pcap header (little-endian, Ethernet link type)
    out.write_all(&pcap_global_header())?;
    out.flush()?;

    let mut buf = Vec::new();
    loop {
        // 4-byte BE length + plist blob
        let mut len_buf = [0u8; 4];
        match read_exact_soft(&mut stream, &mut len_buf) {
            Ok(false) => break,
            Ok(true)  => {}
            Err(e)    => return Err(e.into()),
        }
        let msg_len = u32::from_be_bytes(len_buf) as usize;
        if msg_len == 0 || msg_len > 4 * 1024 * 1024 { break; }

        buf.resize(msg_len, 0);
        match read_exact_soft(&mut stream, &mut buf) {
            Ok(false) => break,
            Ok(true)  => {}
            Err(e)    => return Err(e.into()),
        }

        // The plist value is a Data blob containing the binary record.
        let blob: Vec<u8> = match plist::from_bytes::<Value>(&buf) {
            Ok(Value::Data(b)) => b,
            Ok(_) | Err(_)     => continue, // skip malformed
        };

        if let Some(rec) = parse_record(&blob) {
            let fake_eth = rec.frame_pre_length == 0 && !rec.frame.is_empty();
            let extra = if fake_eth { FAKE_ETH.len() } else { 0 };
            let total  = extra + rec.frame.len();
            let pkt_hdr = pcap_packet_header(rec.ts_sec, rec.ts_usec, total as u32);
            if out.write_all(&pkt_hdr)
                .and_then(|_| if fake_eth { out.write_all(&FAKE_ETH) } else { Ok(()) })
                .and_then(|_| out.write_all(rec.frame))
                .and_then(|_| out.flush())
                .is_err()
            {
                break; // broken pipe (Wireshark closed, etc.)
            }
        }
    }
    Ok(())
}

// ── binary record parser ──────────────────────────────────────────────────────

struct Record<'a> {
    ts_sec:           u32,
    ts_usec:          u32,
    frame_pre_length: u32,
    frame:            &'a [u8],
}

fn parse_record(b: &[u8]) -> Option<Record<'_>> {
    if b.len() < 95 { return None; }
    let header_length = u32::from_be_bytes(b[0..4].try_into().ok()?) as usize;
    let packet_length = u32::from_be_bytes(b[5..9].try_into().ok()?) as usize;
    let frame_pre_length = u32::from_be_bytes(b[17..21].try_into().ok()?);
    let ts_sec  = u32::from_be_bytes(b[87..91].try_into().ok()?);
    let ts_usec = u32::from_be_bytes(b[91..95].try_into().ok()?);
    if b.len() < header_length + packet_length { return None; }
    let frame = &b[header_length..header_length + packet_length];
    Some(Record { ts_sec, ts_usec, frame_pre_length, frame })
}

// ── pcap helpers ──────────────────────────────────────────────────────────────

fn pcap_global_header() -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..4].copy_from_slice(&0xa1b2c3d4u32.to_le_bytes()); // magic
    h[4..6].copy_from_slice(&2u16.to_le_bytes());           // version major
    h[6..8].copy_from_slice(&4u16.to_le_bytes());           // version minor
    // thiszone=0, sigfigs=0 already zero
    h[16..20].copy_from_slice(&65535u32.to_le_bytes());     // snaplen
    h[20..24].copy_from_slice(&1u32.to_le_bytes());         // Ethernet
    h
}

fn pcap_packet_header(ts_sec: u32, ts_usec: u32, len: u32) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0..4].copy_from_slice(&ts_sec.to_le_bytes());
    h[4..8].copy_from_slice(&ts_usec.to_le_bytes());
    h[8..12].copy_from_slice(&len.to_le_bytes());   // incl_len
    h[12..16].copy_from_slice(&len.to_le_bytes());  // orig_len
    h
}

fn read_exact_soft(s: &mut ios_rs::usbmux::MuxSocket, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut done = 0;
    while done < buf.len() {
        match s.read(&mut buf[done..]) {
            Ok(0) => return Ok(false),
            Ok(n) => done += n,
            Err(e) if matches!(e.kind(),
                std::io::ErrorKind::Interrupted |
                std::io::ErrorKind::BrokenPipe  |
                std::io::ErrorKind::ConnectionReset |
                std::io::ErrorKind::UnexpectedEof) => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}
