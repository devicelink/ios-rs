//! Apple File Conduit (`com.apple.afc`) — minimal client for IPA staging.
//!
//! Only implements the operations needed to upload a file:
//! `mkdir`, `file_open`, `file_write`, `file_close`.
use std::io::{Read, Write};

use crate::usbmux::MuxSocket;

use crate::lockdown::{Error, LockdownSession};

const SERVICE: &str = "com.apple.afc";

// ── AFC opcodes ───────────────────────────────────────────────────────────────

const OP_STATUS:        u64 = 1;
#[allow(dead_code)]
const OP_DATA:          u64 = 2;
const OP_MAKE_DIR:      u64 = 9;
const OP_FILE_OPEN:     u64 = 0x0d;
const OP_FILE_OPEN_RES: u64 = 0x0e;
// 0x0f = FileRefRead (not used here)
const OP_FILE_WRITE:    u64 = 0x10;  // FileRefWrite
const OP_FILE_CLOSE:    u64 = 0x14;  // FileRefClose

// File-open mode: create or truncate for writing
const FOPEN_WR: u64 = 4;

// AFC status codes
const STATUS_SUCCESS: u64 = 0;
const STATUS_OBJECT_EXISTS: u64 = 17;

const MAGIC: &[u8] = b"CFA6LPAA";
const HEADER_SIZE: usize = 40;

// ── client ────────────────────────────────────────────────────────────────────

pub struct AfcClient {
    stream:     MuxSocket,
    packet_num: u64,
}

impl AfcClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(AfcClient {
            stream:     session.connect_service(SERVICE)?,
            packet_num: 0,
        })
    }

    // ── public API ────────────────────────────────────────────────────────────

    /// Create a directory (ignores "already exists" errors).
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        let args = nul_str(path);
        self.send_request(OP_MAKE_DIR, &args, &[])?;
        let (op, data) = self.recv_response()?;
        if op == OP_STATUS {
            let code = le_u64(&data, 0);
            if code == STATUS_SUCCESS || code == STATUS_OBJECT_EXISTS {
                return Ok(());
            }
            return Err(Error::Afc(format!("mkdir {path}: AFC error {code}")));
        }
        Err(Error::Afc(format!("mkdir: unexpected op {op:#x}")))
    }

    /// Open a remote file for writing. Returns the file handle.
    pub fn file_open(&mut self, path: &str) -> Result<u64, Error> {
        let mut args = Vec::new();
        args.extend_from_slice(&FOPEN_WR.to_le_bytes());
        args.extend_from_slice(&nul_str(path));
        self.send_request(OP_FILE_OPEN, &args, &[])?;
        let (op, data) = self.recv_response()?;
        if op == OP_FILE_OPEN_RES {
            return Ok(le_u64(&data, 0));
        }
        if op == OP_STATUS {
            return Err(Error::Afc(format!("file_open {path}: AFC error {}", le_u64(&data, 0))));
        }
        Err(Error::Afc(format!("file_open: unexpected op {op:#x}")))
    }

    /// Write data to an open file.
    pub fn file_write(&mut self, handle: u64, data: &[u8]) -> Result<(), Error> {
        let args = handle.to_le_bytes().to_vec();
        self.send_request(OP_FILE_WRITE, &args, data)?;
        let (op, resp) = self.recv_response()?;
        if op == OP_STATUS {
            let code = le_u64(&resp, 0);
            if code == STATUS_SUCCESS { return Ok(()); }
            return Err(Error::Afc(format!("file_write: AFC error {code}")));
        }
        Err(Error::Afc(format!("file_write: unexpected op {op:#x}")))
    }

    /// Close an open file handle.
    pub fn file_close(&mut self, handle: u64) -> Result<(), Error> {
        let args = handle.to_le_bytes().to_vec();
        self.send_request(OP_FILE_CLOSE, &args, &[])?;
        let (op, resp) = self.recv_response()?;
        if op == OP_STATUS {
            let code = le_u64(&resp, 0);
            if code == STATUS_SUCCESS { return Ok(()); }
            return Err(Error::Afc(format!("file_close: AFC error {code}")));
        }
        Err(Error::Afc(format!("file_close: unexpected op {op:#x}")))
    }

    /// Upload `data` to `remote_path`, creating parent directories as needed.
    ///
    /// Writes in 256 KB chunks to avoid memory pressure on large IPAs.
    pub fn put_file(&mut self, remote_path: &str, data: &[u8]) -> Result<(), Error> {
        // Ensure /PublicStaging exists
        if let Some(parent) = remote_path.rfind('/') {
            let dir = &remote_path[..parent];
            if !dir.is_empty() {
                self.mkdir(dir)?;
            }
        }
        let handle = self.file_open(remote_path)?;
        const CHUNK: usize = 256 * 1024;
        for chunk in data.chunks(CHUNK) {
            if let Err(e) = self.file_write(handle, chunk) {
                let _ = self.file_close(handle);
                return Err(e);
            }
        }
        self.file_close(handle)
    }

    // ── internals ─────────────────────────────────────────────────────────────

    fn send_request(&mut self, op: u64, args: &[u8], file_data: &[u8]) -> Result<(), Error> {
        // this_length = header (40) + args
        // entire_length = this_length + file_data
        let this_length   = (HEADER_SIZE + args.len()) as u64;
        let entire_length = this_length + file_data.len() as u64;
        self.packet_num  += 1;

        let mut hdr = [0u8; HEADER_SIZE];
        hdr[0..8].copy_from_slice(MAGIC);
        hdr[8..16].copy_from_slice(&entire_length.to_le_bytes());
        hdr[16..24].copy_from_slice(&this_length.to_le_bytes());
        hdr[24..32].copy_from_slice(&self.packet_num.to_le_bytes());
        hdr[32..40].copy_from_slice(&op.to_le_bytes());

        self.stream.write_all(&hdr)?;
        if !args.is_empty()      { self.stream.write_all(args)?; }
        if !file_data.is_empty() { self.stream.write_all(file_data)?; }
        self.stream.flush()?;
        Ok(())
    }

    /// Returns (operation, payload_bytes)
    fn recv_response(&mut self) -> Result<(u64, Vec<u8>), Error> {
        let mut hdr = [0u8; HEADER_SIZE];
        read_exact(&mut self.stream, &mut hdr)?;

        if &hdr[0..8] != MAGIC {
            return Err(Error::Afc("bad AFC magic in response".into()));
        }
        let entire_length = le_u64(&hdr, 8)  as usize;
        let this_length   = le_u64(&hdr, 16) as usize;
        let op            = le_u64(&hdr, 32);

        let args_len = this_length.saturating_sub(HEADER_SIZE);
        let data_len = entire_length.saturating_sub(this_length);

        let mut payload = vec![0u8; args_len + data_len];
        if !payload.is_empty() {
            read_exact(&mut self.stream, &mut payload)?;
        }
        Ok((op, payload))
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn nul_str(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn le_u64(buf: &[u8], offset: usize) -> u64 {
    if buf.len() < offset + 8 { return 0; }
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

fn read_exact(s: &mut MuxSocket, buf: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = s.read(&mut buf[filled..])?;
        if n == 0 { return Err(Error::Closed); }
        filled += n;
    }
    Ok(())
}
