//! Syslog relay client (`com.apple.syslog_relay`).
//!
//! Streams live syslog entries from the device.  Entries are NUL-terminated
//! UTF-8 strings in the classic Apple System Log format:
//!
//! ```text
//! Apr  5 12:34:56 iPhone kernel[0] <Notice>: message here
//! ```
//!
//! # Quick start
//!
//! ```ignore
//! let mut syslog = SyslogClient::connect(&mut lockdown_session)?;
//! syslog.stream(|entry| {
//!     println!("{}", entry.raw);
//!     true  // return false to stop
//! })?;
//! ```
use std::io::Read;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.syslog_relay";
const BUF_SIZE: usize = 4 * 1024;

// ── public types ──────────────────────────────────────────────────────────────

/// A parsed syslog entry.
#[derive(Debug, Clone)]
pub struct SyslogEntry {
    /// Full raw line as received from the device.
    pub raw: String,
    /// Parsed timestamp string (e.g. `"Apr  5 12:34:56"`).
    pub timestamp: String,
    /// Process name and PID (e.g. `"kernel[0]"`).
    pub process: String,
    /// Log level (e.g. `"Notice"`, `"Error"`, `"Warning"`).
    pub level: String,
    /// The actual log message.
    pub message: String,
}

impl SyslogEntry {
    fn parse(raw: String) -> Self {
        // Format: "Mmm [ D]D HH:MM:SS[.mmm] hostname process[pid] <Level>: message"
        // Single-digit days are padded with a space: "Apr  5" — use split_whitespace
        // to skip multiple consecutive spaces, then reconstruct the rest from the tail.
        let mut iter = raw.split_whitespace();
        let month = iter.next().unwrap_or("");
        let day = iter.next().unwrap_or("");
        let time = iter.next().unwrap_or("");
        let _host = iter.next();
        let proc = iter.next().unwrap_or("").to_string();
        let rest = iter.collect::<Vec<_>>().join(" ");

        let timestamp = if month.is_empty() {
            String::new()
        } else {
            format!("{month} {day:>2} {time}")
        };

        // rest = "<Level>: message" or just the message
        let (level, message) = if let Some(stripped) = rest.strip_prefix('<') {
            if let Some(end) = stripped.find('>') {
                let lv = stripped[..end].to_string();
                let msg = stripped[end + 1..].trim_start_matches(": ").to_string();
                (lv, msg)
            } else {
                (String::new(), rest.clone())
            }
        } else {
            (String::new(), rest.clone())
        };

        SyslogEntry {
            raw,
            timestamp,
            process: proc,
            level,
            message,
        }
    }
}

// ── client ────────────────────────────────────────────────────────────────────

/// Syslog relay client.
///
/// Obtain via [`SyslogClient::connect`] (lockdownd) or
/// [`SyslogClient::from_stream`] (RSD shim on iOS 17.4+).
pub struct SyslogClient {
    stream: MuxSocket,
}

impl SyslogClient {
    /// Connect via lockdownd.  Works on all iOS versions.
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(SyslogClient {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Build from a pre-connected stream (e.g. from
    /// `DeviceSession::connect_rsd_shim("com.apple.syslog_relay.shim.remote")`).
    pub fn from_stream(stream: MuxSocket) -> Self {
        SyslogClient { stream }
    }

    /// Stream log entries, calling `callback` for each one.
    ///
    /// The callback receives a [`SyslogEntry`] and returns `true` to continue
    /// or `false` to stop.  Returns when the connection closes, the callback
    /// returns `false`, or an I/O error occurs that isn't a clean interruption.
    pub fn stream(&mut self, mut callback: impl FnMut(SyslogEntry) -> bool) -> Result<(), Error> {
        let mut buf = vec![0u8; BUF_SIZE];
        let mut carry = Vec::new();

        loop {
            let n = match self.stream.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if is_interrupted(&e) => return Ok(()),
                Err(e) => return Err(Error::Io(e)),
            };

            carry.extend_from_slice(&buf[..n]);

            // Entries are NUL-terminated; some older iOS versions use '\n'.
            while let Some(pos) = carry.iter().position(|&b| b == 0 || b == b'\n') {
                let raw_bytes: Vec<u8> = carry.drain(..pos + 1).collect();
                let raw = String::from_utf8_lossy(&raw_bytes[..raw_bytes.len().saturating_sub(1)])
                    .trim()
                    .to_string();
                if raw.is_empty() {
                    continue;
                }
                let entry = SyslogEntry::parse(raw);
                if !callback(entry) {
                    return Ok(());
                }
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns true for I/O errors that represent a clean shutdown (Ctrl-C, pipe
/// closed, connection reset after the device is unplugged, etc.).
fn is_interrupted(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        Interrupted | BrokenPipe | ConnectionReset | UnexpectedEof
    )
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_line() {
        let raw = "Apr  5 12:34:56 iPhone SpringBoard[1234] <Notice>: some log message".to_string();
        let e = SyslogEntry::parse(raw);
        assert_eq!(e.timestamp, "Apr  5 12:34:56");
        assert_eq!(e.process, "SpringBoard[1234]");
        assert_eq!(e.level, "Notice");
        assert_eq!(e.message, "some log message");
    }

    #[test]
    fn parse_error_level() {
        let raw = "Dec 15 08:00:01 iPhone kernel[0] <Error>: panic!".to_string();
        let e = SyslogEntry::parse(raw);
        assert_eq!(e.level, "Error");
        assert_eq!(e.message, "panic!");
    }

    #[test]
    fn parse_no_level() {
        let raw =
            "Dec 15 08:00:01 iPhone someproc[99] something without angle brackets".to_string();
        let e = SyslogEntry::parse(raw);
        assert!(e.level.is_empty());
        assert!(e.message.contains("something"));
    }

    #[test]
    fn parse_empty_is_stable() {
        let e = SyslogEntry::parse(String::new());
        assert!(e.raw.is_empty());
    }
}
