//! Structured os_log streaming via `com.apple.os_trace_relay.shim.remote`.
//!
//! Protocol after RSDCheckin:
//!   1. Send 4-byte BE length + XML plist: {Request: StartActivity, Pid: -1,
//!      MessageFilter: 0xFFFF, StreamFlags: 0x20}
//!   2. Read response: 4-byte LE (lengthLength) → lengthLength bytes reversed
//!      as BE integer = plist byte count → plist → check Status == RequestSuccessful
//!   3. Loop: 0x02 byte + 4-byte LE length + entry bytes → parse binary log entry
//!
//! Entry binary layout (all LE, packed):
//!   [0]      u8    marker
//!   [1..5]   u32   type
//!   [5..9]   u32   headerSize
//!   [9..13]  u32   pid
//!   [37..39] u16   procpathLen
//!   [55..63] u64   timeSec
//!   [63..67] u32   timeUsec
//!   [68]     u8    level  (0=default,1=info,2=debug,0x10=error,0x11=fault)
//!   [107..109] u16 imagepathLen
//!   [109..113] u32 messageLen
//!   [117..119] u16 subsystemLen
//!   [121..123] u16 categoryLen
//!   [129+]  var   procpath, imagepath, message, subsystem, category (cstrings)
use std::io::{Read, Write};

use anyhow::{Context, Result};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

const SHIM: &str = "com.apple.os_trace_relay.shim.remote";

pub fn run(
    udid:    Option<&str>,
    process: Option<&str>,
    level:   Option<&str>,
    json:    bool,
) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let mut stream = session.connect_rsd_shim(SHIM).context("connect os_trace shim")?;

    // Send StartActivity request
    {
        let mut req = plist::Dictionary::new();
        req.insert("Request".into(),       plist::Value::String("StartActivity".into()));
        req.insert("Pid".into(),           plist::Value::Integer((-1i64).into()));
        req.insert("MessageFilter".into(), plist::Value::Integer(0xFFFFi64.into()));
        req.insert("StreamFlags".into(),   plist::Value::Integer(0x20i64.into()));
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, &plist::Value::Dictionary(req))?;
        let len = body.len() as u32;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;
    }

    // Read handshake response
    {
        let mut ll_buf = [0u8; 4];
        read_exact(&mut stream, &mut ll_buf).context("read length-length")?;
        let length_length = u32::from_le_bytes(ll_buf) as usize;
        let mut len_bytes = vec![0u8; length_length];
        read_exact(&mut stream, &mut len_bytes).context("read plist length bytes")?;
        len_bytes.reverse();
        let mut plist_len: u64 = 0;
        for b in &len_bytes { plist_len = (plist_len << 8) | *b as u64; }
        let mut plist_buf = vec![0u8; plist_len as usize];
        read_exact(&mut stream, &mut plist_buf).context("read StartActivity response")?;
        let resp: plist::Value = plist::from_bytes(&plist_buf)?;
        if let plist::Value::Dictionary(d) = &resp {
            let status = d.get("Status").and_then(|v| v.as_string()).unwrap_or("");
            if status != "RequestSuccessful" {
                anyhow::bail!("StartActivity failed: {resp:?}");
            }
        }
    }

    let min_level = parse_level(level.unwrap_or("default"));
    let mut stdout = std::io::BufWriter::new(std::io::stdout());

    loop {
        let entry = match read_entry(&mut stream) {
            Ok(e)  => e,
            Err(_) => break,
        };
        if entry.level_num < min_level { continue; }
        if let Some(p) = process {
            if !entry.process.to_lowercase().contains(&p.to_lowercase()) { continue; }
        }

        let line = if json {
            format!(
                r#"{{"ts":"{ts}","pid":{pid},"level":"{lv}","process":{proc},"subsystem":{sub},"category":{cat},"message":{msg}}}"#,
                ts   = entry.timestamp,
                pid  = entry.pid,
                lv   = entry.level,
                proc = json_str(&entry.process),
                sub  = json_str(&entry.subsystem),
                cat  = json_str(&entry.category),
                msg  = json_str(&entry.message),
            )
        } else {
            format!(
                "{ts}  {pid:>6}  {lv:<8}  {proc:<20}  {msg}",
                ts   = entry.timestamp,
                pid  = entry.pid,
                lv   = entry.level,
                proc = truncate(&entry.process, 20),
                msg  = entry.message,
            )
        };

        if writeln!(stdout, "{line}").and_then(|_| stdout.flush()).is_err() {
            break;
        }
    }
    Ok(())
}

// ── entry parsing ─────────────────────────────────────────────────────────────

struct LogEntry {
    pid:       u32,
    timestamp: String,
    level:     String,
    level_num: u8,
    process:   String,
    subsystem: String,
    category:  String,
    message:   String,
}

fn read_entry(s: &mut ios_rs::usbmux::MuxSocket) -> std::io::Result<LogEntry> {
    let mut magic = [0u8; 1];
    read_exact(s, &mut magic)?;
    if magic[0] != 0x02 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad magic"));
    }
    let mut len_buf = [0u8; 4];
    read_exact(s, &mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut data = vec![0u8; len];
    read_exact(s, &mut data)?;
    parse_entry(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn parse_entry(data: &[u8]) -> anyhow::Result<LogEntry> {
    if data.len() < 129 {
        anyhow::bail!("entry too short: {} bytes", data.len());
    }
    let pid           = u32::from_le_bytes(data[9..13].try_into()?);
    let procpath_len  = u16::from_le_bytes(data[37..39].try_into()?) as usize;
    let time_sec      = u64::from_le_bytes(data[55..63].try_into()?);
    let time_usec     = u32::from_le_bytes(data[63..67].try_into()?);
    let level         = data[68];
    let imagepath_len = u16::from_le_bytes(data[107..109].try_into()?) as usize;
    let message_len   = u32::from_le_bytes(data[109..113].try_into()?) as usize;
    let subsystem_len = u16::from_le_bytes(data[117..119].try_into()?) as usize;
    let category_len  = u16::from_le_bytes(data[121..123].try_into()?) as usize;

    let ts = format_timestamp(time_sec, time_usec);

    let mut off = 129;
    let process   = read_cstring(&data, &mut off, procpath_len);
    let _image    = read_cstring(&data, &mut off, imagepath_len);
    let message   = read_cstring(&data, &mut off, message_len);
    let subsystem = read_cstring(&data, &mut off, subsystem_len);
    let category  = read_cstring(&data, &mut off, category_len);

    let process = basename(&process);

    Ok(LogEntry {
        pid,
        timestamp: ts,
        level: level_name(level).into(),
        level_num: level,
        process,
        subsystem,
        category,
        message,
    })
}

fn read_cstring(data: &[u8], off: &mut usize, len: usize) -> String {
    if len == 0 || *off + len > data.len() { return String::new(); }
    let slice = &data[*off .. *off + len];
    *off += len;
    let s = if slice.last() == Some(&0) { &slice[..slice.len()-1] } else { slice };
    String::from_utf8_lossy(s).into_owned()
}

fn level_name(l: u8) -> &'static str {
    match l {
        0x00 => "Default",
        0x01 => "Info",
        0x02 => "Debug",
        0x10 => "Error",
        0x11 => "Fault",
        _    => "Unknown",
    }
}

fn parse_level(s: &str) -> u8 {
    match s.to_lowercase().as_str() {
        "debug"   => 0x00,
        "info"    => 0x00,
        "default" => 0x00,
        "error"   => 0x10,
        "fault"   => 0x11,
        _         => 0x00,
    }
}

fn format_timestamp(sec: u64, usec: u32) -> String {
    // Seconds since 2001-01-01 (CoreData epoch) → Unix
    const APPLE_EPOCH_OFFSET: u64 = 978_307_200;
    let unix = sec.saturating_add(APPLE_EPOCH_OFFSET);
    let dt = std::time::UNIX_EPOCH + std::time::Duration::new(unix, usec * 1000);
    let secs = dt.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{usec:06}")
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { format!("{s:<n$}") } else { format!("{}…", &s[..n-1]) }
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn read_exact(s: &mut ios_rs::usbmux::MuxSocket, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match s.read(&mut buf[done..]) {
            Ok(0)  => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")),
            Ok(n)  => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
