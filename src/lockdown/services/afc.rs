//! Apple File Conduit (`com.apple.afc`) client.
//!
//! Exposes the iOS media-partition file system (Photos, Podcasts, Books, …)
//! over the AFC binary protocol.  Connect via lockdownd for the standard
//! media partition, or via the house-arrest service for per-app containers.
//!
//! # Quick start
//!
//! ```ignore
//! let mut afc = AfcClient::connect(&mut lockdown_session)?;
//! let entries = afc.list_dir("/")?;
//! let info    = afc.device_info()?;
//! afc.pull_file("/Books/book.epub", Path::new("book.epub"), |_, _| {})?;
//! afc.push_file(Path::new("photo.jpg"), "/DCIM/photo.jpg")?;
//! ```
use std::io::{Read, Write};
use std::path::Path;

use crate::usbmux::MuxSocket;
use crate::lockdown::{Error, LockdownSession};

const SERVICE: &str = "com.apple.afc";
const MAGIC:       &[u8] = b"CFA6LPAA";
const HEADER_SIZE: usize = 40;
const READ_CHUNK:  usize = 4 * 1024 * 1024; // 4 MB per read request
const WRITE_CHUNK: usize = 256 * 1024;       // 256 KB per write request

// ── opcodes ───────────────────────────────────────────────────────────────────

const OP_STATUS:        u64 = 0x01;
const OP_DATA:          u64 = 0x02;
const OP_READ_DIR:      u64 = 0x03;
const OP_REMOVE_PATH:   u64 = 0x08;
const OP_MAKE_DIR:      u64 = 0x09;
const OP_GET_FILE_INFO: u64 = 0x0a;
const OP_GET_DEVINFO:   u64 = 0x0b;
const OP_FILE_OPEN:     u64 = 0x0d;
const OP_FILE_OPEN_RES: u64 = 0x0e;
const OP_FILE_READ:     u64 = 0x0f;
const OP_FILE_WRITE:    u64 = 0x10;
const OP_FILE_CLOSE:    u64 = 0x14;
const OP_RENAME_PATH:   u64 = 0x22;
const OP_MAKE_LINK:     u64 = 0x28;

// ── file-open modes ───────────────────────────────────────────────────────────

const FOPEN_RDONLY: u64 = 0x01;
const FOPEN_WR:     u64 = 0x04; // create or truncate

// ── status codes ─────────────────────────────────────────────────────────────

const STATUS_SUCCESS:        u64 = 0;
const STATUS_OBJECT_EXISTS:  u64 = 16;
const STATUS_END_OF_DATA:    u64 = 14;

// ── public types ──────────────────────────────────────────────────────────────

/// Type of a file-system entry returned by [`AfcClient::get_file_info`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

impl FileType {
    fn from_str(s: &str) -> Self {
        match s {
            "S_IFREG" => FileType::Regular,
            "S_IFDIR" => FileType::Directory,
            "S_IFLNK" => FileType::Symlink,
            _          => FileType::Other,
        }
    }

    pub fn indicator(&self) -> char {
        match self {
            FileType::Regular    => '-',
            FileType::Directory  => 'd',
            FileType::Symlink    => 'l',
            FileType::Other      => '?',
        }
    }
}

/// Metadata for a single file-system entry.
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// Entry name (leaf, not the full path).
    pub name:          String,
    pub file_type:     FileType,
    /// File size in bytes (0 for directories).
    pub size:          u64,
    /// Modification time in nanoseconds since Unix epoch.
    pub modified_nanos: u64,
    /// Symlink target (only set when `file_type == Symlink`).
    pub link_target:   Option<String>,
}

impl FileInfo {
    pub fn parse(name: impl Into<String>, data: &[u8]) -> Self {
        let pairs = parse_kv_pairs(data);
        let file_type = pairs.get("st_ifmt")
            .map(|s| FileType::from_str(s))
            .unwrap_or(FileType::Other);
        let size = pairs.get("st_size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let modified_nanos = pairs.get("st_mtime")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let link_target = pairs.get("LinkTarget").cloned();
        FileInfo { name: name.into(), file_type, size, modified_nanos, link_target }
    }
}

/// Summary information about the device's file system.
#[derive(Debug, Clone)]
pub struct AfcDeviceInfo {
    pub model:       String,
    pub total_bytes: u64,
    pub free_bytes:  u64,
    pub block_size:  u64,
}

impl AfcDeviceInfo {
    pub fn parse(data: &[u8]) -> Self {
        let pairs = parse_kv_pairs(data);
        AfcDeviceInfo {
            model:       pairs.get("Model").cloned().unwrap_or_default(),
            total_bytes: pairs.get("FSTotalBytes").and_then(|s| s.parse().ok()).unwrap_or(0),
            free_bytes:  pairs.get("FSFreeBytes").and_then(|s| s.parse().ok()).unwrap_or(0),
            block_size:  pairs.get("FSBlockSize").and_then(|s| s.parse().ok()).unwrap_or(0),
        }
    }
}

// ── client ────────────────────────────────────────────────────────────────────

/// AFC protocol client.
///
/// Obtain one via [`AfcClient::connect`] (lockdownd) or
/// [`AfcClient::from_stream`] (any pre-connected `MuxSocket`, e.g. from the
/// house-arrest service or an RSD shim).
pub struct AfcClient {
    stream:     MuxSocket,
    packet_num: u64,
}

impl AfcClient {
    /// Connect to the standard AFC service (`com.apple.afc`) through lockdownd.
    ///
    /// Gives access to the media partition (`/` = `DCIM`, `Books`, etc.).
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(AfcClient { stream: session.connect_service(SERVICE)?, packet_num: 0 })
    }

    /// Build an `AfcClient` from any already-connected `MuxSocket`.
    ///
    /// Use this when you obtained the socket through the house-arrest service
    /// (per-app container), an RSD shim, or any other transport.
    pub fn from_stream(stream: MuxSocket) -> Self {
        AfcClient { stream, packet_num: 0 }
    }

    /// Connect to an app's full sandbox container via `com.apple.mobile.house_arrest`.
    ///
    /// Uses the legacy lockdownd path.  On iOS 17.4+ you should call
    /// [`Self::connect_app_shim`] with a stream obtained from
    /// `DeviceSession::connect_rsd_shim("com.apple.mobile.house_arrest.shim.remote")`.
    pub fn connect_app(session: &mut LockdownSession, bundle_id: &str) -> Result<Self, Error> {
        let stream = session.connect_service("com.apple.mobile.house_arrest")?;
        Self::house_arrest_on_stream(stream, "VendContainer", bundle_id)
    }

    /// Connect to an app's `Documents/` directory — legacy lockdownd path.
    pub fn connect_app_documents(
        session:   &mut LockdownSession,
        bundle_id: &str,
    ) -> Result<Self, Error> {
        let stream = session.connect_service("com.apple.mobile.house_arrest")?;
        Self::house_arrest_on_stream(stream, "VendDocuments", bundle_id)
    }

    /// Connect to an app's full sandbox container over an already-connected
    /// `MuxSocket` (e.g. from
    /// `DeviceSession::connect_rsd_shim("com.apple.mobile.house_arrest.shim.remote")`).
    ///
    /// Use this on iOS 17.4+ where the house_arrest service is only reachable
    /// via the RSD shim.
    pub fn connect_app_shim(stream: MuxSocket, bundle_id: &str) -> Result<Self, Error> {
        Self::house_arrest_on_stream(stream, "VendContainer", bundle_id)
    }

    /// Like [`Self::connect_app_shim`] but scoped to `Documents/` only.
    pub fn connect_app_documents_shim(stream: MuxSocket, bundle_id: &str) -> Result<Self, Error> {
        Self::house_arrest_on_stream(stream, "VendDocuments", bundle_id)
    }

    fn house_arrest_on_stream(
        mut stream: MuxSocket,
        command:    &str,
        bundle_id:  &str,
    ) -> Result<Self, Error> {
        // Send the VendContainer / VendDocuments plist
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, &plist::Value::Dictionary({
            let mut d = plist::Dictionary::new();
            d.insert("Command".into(),    plist::Value::String(command.into()));
            d.insert("Identifier".into(), plist::Value::String(bundle_id.into()));
            d
        }))?;
        let len = body.len() as u32;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;

        // Read the response (4-byte BE length prefix + plist).
        // iOS may close the TLS connection without a close_notify after the
        // response — treat UnexpectedEof as EOF rather than an error.
        let mut len_buf = [0u8; 4];
        read_exact_eof_ok(&mut stream, &mut len_buf)?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        if resp_len == 0 || resp_len > 1024 * 1024 {
            return Err(Error::Afc(format!(
                "house_arrest({bundle_id}): service closed without response. \
                 On iOS 17.4+ use connect_app_shim() with the RSD shim stream."
            )));
        }
        let mut resp_body = vec![0u8; resp_len];
        read_exact_eof_ok(&mut stream, &mut resp_body)?;

        let resp: plist::Value = plist::from_bytes(&resp_body)?;
        if let Some(err) = resp.as_dictionary()
            .and_then(|d| d.get("Error"))
            .and_then(|v| v.as_string())
        {
            let desc = resp.as_dictionary()
                .and_then(|d| d.get("ErrorDescription"))
                .and_then(|v| v.as_string())
                .unwrap_or(err);
            return Err(Error::Afc(format!("house_arrest({bundle_id}): {desc}")));
        }

        Ok(AfcClient { stream, packet_num: 0 })
    }

    // ── directory operations ─────────────────────────────────────────────────

    /// List the names of entries in `path`.
    ///
    /// Returns an error if `path` is not a directory.
    pub fn list_dir(&mut self, path: &str) -> Result<Vec<String>, Error> {
        self.send_request(OP_READ_DIR, &nul_str(path), &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_DATA   => Ok(parse_nul_strings(&data).into_iter()
                            .filter(|n| n != "." && n != "..").collect()),
            OP_STATUS => Err(Error::Afc(format!(
                "list_dir {path}: {}", status_name(le_u64(&data, 0))))),
            _         => Err(Error::Afc(format!("list_dir: unexpected op {op:#x}"))),
        }
    }

    /// Return metadata for `path`.
    pub fn get_file_info(&mut self, path: &str) -> Result<FileInfo, Error> {
        self.send_request(OP_GET_FILE_INFO, &nul_str(path), &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_DATA   => Ok(FileInfo::parse(leaf(path), &data)),
            OP_STATUS => Err(Error::Afc(format!(
                "stat {path}: {}", status_name(le_u64(&data, 0))))),
            _         => Err(Error::Afc(format!("stat: unexpected op {op:#x}"))),
        }
    }

    /// Return information about the device's file system (model, free space, …).
    pub fn device_info(&mut self) -> Result<AfcDeviceInfo, Error> {
        self.send_request(OP_GET_DEVINFO, &[], &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_DATA   => Ok(AfcDeviceInfo::parse(&data)),
            OP_STATUS => Err(Error::Afc(format!(
                "device_info: {}", status_name(le_u64(&data, 0))))),
            _         => Err(Error::Afc(format!("device_info: unexpected op {op:#x}"))),
        }
    }

    /// Create a directory at `path`.  Silently succeeds if it already exists.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        self.send_request(OP_MAKE_DIR, &nul_str(path), &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_STATUS => {
                let code = le_u64(&data, 0);
                if code == STATUS_SUCCESS || code == STATUS_OBJECT_EXISTS { Ok(()) }
                else { Err(Error::Afc(format!("mkdir {path}: {}", status_name(code)))) }
            }
            _ => Err(Error::Afc(format!("mkdir: unexpected op {op:#x}"))),
        }
    }

    // ── path operations ──────────────────────────────────────────────────────

    /// Delete the file or empty directory at `path`.
    pub fn remove_path(&mut self, path: &str) -> Result<(), Error> {
        self.send_request(OP_REMOVE_PATH, &nul_str(path), &[])?;
        self.expect_status(&format!("remove {path}"))
    }

    /// Rename or move `from` to `to`.
    ///
    /// **Note:** on the standard AFC media-partition service (`com.apple.afc`)
    /// iOS requires the destination path to already exist; renaming to a
    /// non-existent path silently removes the source without creating the
    /// destination.  This is an iOS restriction, not a protocol bug.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        let mut args = nul_str(from);
        args.extend_from_slice(&nul_str(to));
        self.send_request(OP_RENAME_PATH, &args, &[])?;
        self.expect_status(&format!("rename {from} -> {to}"))
    }

    /// Create a symlink (`hard = false`) or hard link (`hard = true`).
    ///
    /// `target` is what the link points to; `link_name` is the path of the
    /// new link entry.
    pub fn make_link(&mut self, hard: bool, target: &str, link_name: &str) -> Result<(), Error> {
        let link_type: u64 = if hard { 1 } else { 2 };
        let mut args = link_type.to_le_bytes().to_vec();
        args.extend_from_slice(&nul_str(target));
        args.extend_from_slice(&nul_str(link_name));
        self.send_request(OP_MAKE_LINK, &args, &[])?;
        self.expect_status(&format!("make_link {link_name}"))
    }

    // ── file handle operations ───────────────────────────────────────────────

    /// Open `path` for reading.  Returns a file handle.
    pub fn file_open_read(&mut self, path: &str) -> Result<u64, Error> {
        self.open_with_mode(path, FOPEN_RDONLY)
    }

    /// Open `path` for writing (create or truncate).  Returns a file handle.
    ///
    /// This is the original write-open used by [`Self::put_file`].
    pub fn file_open(&mut self, path: &str) -> Result<u64, Error> {
        self.open_with_mode(path, FOPEN_WR)
    }

    /// Read up to `size` bytes from an open file handle.
    ///
    /// Returns an empty `Vec` when the file is exhausted.
    pub fn file_read(&mut self, handle: u64, size: usize) -> Result<Vec<u8>, Error> {
        let mut args = handle.to_le_bytes().to_vec();
        args.extend_from_slice(&(size as u64).to_le_bytes());
        self.send_request(OP_FILE_READ, &args, &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_DATA   => Ok(data),
            OP_STATUS => {
                let code = le_u64(&data, 0);
                if code == STATUS_SUCCESS || code == STATUS_END_OF_DATA { Ok(vec![]) }
                else { Err(Error::Afc(format!("file_read: {}", status_name(code)))) }
            }
            _ => Err(Error::Afc(format!("file_read: unexpected op {op:#x}"))),
        }
    }

    /// Write `data` to an open file handle.
    pub fn file_write(&mut self, handle: u64, data: &[u8]) -> Result<(), Error> {
        let args = handle.to_le_bytes().to_vec();
        self.send_request(OP_FILE_WRITE, &args, data)?;
        self.expect_status("file_write")
    }

    /// Close an open file handle.
    pub fn file_close(&mut self, handle: u64) -> Result<(), Error> {
        let args = handle.to_le_bytes().to_vec();
        self.send_request(OP_FILE_CLOSE, &args, &[])?;
        self.expect_status("file_close")
    }

    // ── high-level file I/O ──────────────────────────────────────────────────

    /// Read the entire contents of a remote file into memory.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        let handle = self.file_open_read(path)?;
        let mut buf = Vec::new();
        loop {
            let chunk = match self.file_read(handle, READ_CHUNK) {
                Ok(c) => c,
                Err(e) => { let _ = self.file_close(handle); return Err(e); }
            };
            if chunk.is_empty() { break; }
            buf.extend_from_slice(&chunk);
        }
        self.file_close(handle)?;
        Ok(buf)
    }

    /// Download `remote_path` to `local_path`.
    ///
    /// `progress(bytes_done, total_bytes)` is called after each chunk; pass
    /// `|_, _| {}` to ignore progress.
    pub fn pull_file(
        &mut self,
        remote_path: &str,
        local_path:  &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<(), Error> {
        self.pull_file_dyn(remote_path, local_path, &mut progress)
    }

    fn pull_file_dyn(
        &mut self,
        remote_path: &str,
        local_path:  &Path,
        progress:    &mut dyn FnMut(u64, u64),
    ) -> Result<(), Error> {
        let info = self.get_file_info(remote_path)?;

        if info.file_type == FileType::Directory {
            std::fs::create_dir_all(local_path)
                .map_err(|e| Error::Afc(format!("create_dir {}: {e}", local_path.display())))?;
            for entry in self.list_dir(remote_path)? {
                let remote = format!("{}/{}", remote_path.trim_end_matches('/'), entry);
                let local  = local_path.join(&entry);
                self.pull_file_dyn(&remote, &local, progress)?;
            }
            return Ok(());
        }

        let handle = self.file_open_read(remote_path)?;
        let mut out = std::fs::File::create(local_path)
            .map_err(|e| Error::Afc(format!("create {}: {e}", local_path.display())))?;
        let total = info.size;
        let mut done = 0u64;
        loop {
            let chunk = match self.file_read(handle, READ_CHUNK) {
                Ok(c) => c,
                Err(e) => { let _ = self.file_close(handle); return Err(e); }
            };
            if chunk.is_empty() { break; }
            out.write_all(&chunk)
                .map_err(|e| Error::Afc(format!("write local: {e}")))?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        self.file_close(handle)
    }

    /// Upload a local file (or directory tree) to `remote_path`.
    pub fn push_file(&mut self, local_path: &Path, remote_path: &str) -> Result<(), Error> {
        let meta = std::fs::metadata(local_path)
            .map_err(|e| Error::Afc(format!("stat {}: {e}", local_path.display())))?;

        if meta.is_dir() {
            self.mkdir(remote_path)?;
            for entry in std::fs::read_dir(local_path)
                .map_err(|e| Error::Afc(format!("read_dir: {e}")))?
            {
                let entry = entry.map_err(|e| Error::Afc(format!("dir entry: {e}")))?;
                let name  = entry.file_name();
                let remote = format!("{}/{}", remote_path.trim_end_matches('/'),
                                     name.to_string_lossy());
                self.push_file(&entry.path(), &remote)?;
            }
            return Ok(());
        }

        self.put_file_from_path(local_path, remote_path)
    }

    /// Upload raw bytes to `remote_path`, creating parent directories if needed.
    ///
    /// Writes in 256 KB chunks.  This is the original method used by the IPA
    /// installer; it remains the preferred API for small in-memory payloads.
    pub fn put_file(&mut self, remote_path: &str, data: &[u8]) -> Result<(), Error> {
        if let Some(slash) = remote_path.rfind('/') {
            let dir = &remote_path[..slash];
            if !dir.is_empty() { self.mkdir(dir)?; }
        }
        let handle = self.file_open(remote_path)?;
        for chunk in data.chunks(WRITE_CHUNK) {
            if let Err(e) = self.file_write(handle, chunk) {
                let _ = self.file_close(handle);
                return Err(e);
            }
        }
        self.file_close(handle)
    }

    // ── internals ─────────────────────────────────────────────────────────────

    fn open_with_mode(&mut self, path: &str, mode: u64) -> Result<u64, Error> {
        let mut args = mode.to_le_bytes().to_vec();
        args.extend_from_slice(&nul_str(path));
        self.send_request(OP_FILE_OPEN, &args, &[])?;
        let (op, data) = self.recv_response()?;
        match op {
            OP_FILE_OPEN_RES => Ok(le_u64(&data, 0)),
            OP_STATUS => Err(Error::Afc(format!(
                "file_open {path}: {}", status_name(le_u64(&data, 0))))),
            _ => Err(Error::Afc(format!("file_open: unexpected op {op:#x}"))),
        }
    }

    fn put_file_from_path(&mut self, local: &Path, remote_path: &str) -> Result<(), Error> {
        let mut f = std::fs::File::open(local)
            .map_err(|e| Error::Afc(format!("open {}: {e}", local.display())))?;
        let handle = self.file_open(remote_path)?;
        let mut buf = vec![0u8; WRITE_CHUNK];
        loop {
            let n = f.read(&mut buf).map_err(|e| Error::Afc(format!("read local: {e}")))?;
            if n == 0 { break; }
            if let Err(e) = self.file_write(handle, &buf[..n]) {
                let _ = self.file_close(handle);
                return Err(e);
            }
        }
        self.file_close(handle)
    }

    fn expect_status(&mut self, context: &str) -> Result<(), Error> {
        let (op, data) = self.recv_response()?;
        match op {
            OP_STATUS => {
                let code = le_u64(&data, 0);
                if code == STATUS_SUCCESS { Ok(()) }
                else { Err(Error::Afc(format!("{context}: {}", status_name(code)))) }
            }
            _ => Err(Error::Afc(format!("{context}: unexpected op {op:#x}"))),
        }
    }

    fn send_request(&mut self, op: u64, args: &[u8], file_data: &[u8]) -> Result<(), Error> {
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

    fn recv_response(&mut self) -> Result<(u64, Vec<u8>), Error> {
        let mut hdr = [0u8; HEADER_SIZE];
        read_exact(&mut self.stream, &mut hdr)?;

        if &hdr[0..8] != MAGIC {
            return Err(Error::Afc("bad AFC magic in response".into()));
        }
        let entire_length = le_u64(&hdr,  8) as usize;
        let this_length   = le_u64(&hdr, 16) as usize;
        let op            = le_u64(&hdr, 32);

        let args_len = this_length.saturating_sub(HEADER_SIZE);
        let data_len = entire_length.saturating_sub(this_length);
        let mut payload = vec![0u8; args_len + data_len];
        if !payload.is_empty() { read_exact(&mut self.stream, &mut payload)?; }
        Ok((op, payload))
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Return a NUL-terminated copy of `s` as bytes.
pub fn nul_str(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// Read a little-endian `u64` from `buf` at `offset`.
///
/// Returns 0 if `buf` is shorter than `offset + 8`.
pub fn le_u64(buf: &[u8], offset: usize) -> u64 {
    if buf.len() < offset + 8 { return 0; }
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

/// Parse a NUL-separated sequence of strings from `data`.
pub fn parse_nul_strings(data: &[u8]) -> Vec<String> {
    data.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Parse NUL-separated `key\0value\0key\0value\0…` pairs into a map.
pub fn parse_kv_pairs(data: &[u8]) -> std::collections::HashMap<String, String> {
    let strings = parse_nul_strings(data);
    strings.chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0].clone(), c[1].clone()))
        .collect()
}

/// Return the leaf name of a path (the part after the last `/`).
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Human-readable name for an AFC status code.
pub fn status_name(code: u64) -> String {
    let name = match code {
        0  => "success",
        1  => "unknown error",
        2  => "invalid header",
        3  => "no resources",
        4  => "read error",
        5  => "write error",
        6  => "unknown packet type",
        7  => "invalid arg",
        8  => "not found",
        9  => "is a directory",
        10 => "permission denied",
        11 => "not connected",
        12 => "timeout",
        13 => "too much data",
        14 => "end of data",
        15 => "not supported",
        16 => "object exists",
        17 => "object busy",
        18 => "no space left",
        20 => "I/O error",
        _  => "unknown",
    };
    format!("{name} (code {code})")
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

/// Like `read_exact` but treats `UnexpectedEof` as a clean EOF.
///
/// iOS TLS services often close the connection without sending a TLS
/// close_notify alert.  rustls surfaces this as `ErrorKind::UnexpectedEof`;
/// we treat it the same as `n == 0` so callers can inspect how many bytes
/// were actually read.
fn read_exact_eof_ok(s: &mut MuxSocket, buf: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match s.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    // Zero the unread tail so callers can detect a short read.
    buf[filled..].fill(0);
    Ok(())
}
