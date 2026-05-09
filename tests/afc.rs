//! AFC protocol tests.
//!
//! Unit tests cover the codec helpers and wire-format construction; they run
//! without a device.  Integration tests (marked `#[ignore]`) require a real
//! iOS device to be connected.
use ios_rs::lockdown::services::{AfcClient, AfcDeviceInfo, FileInfo, FileType};

fn setup() {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

// ── bring private helpers into scope for unit tests ───────────────────────────

use ios_rs::lockdown::services::afc::{
    le_u64, nul_str, parse_kv_pairs, parse_nul_strings, status_name,
};

// ── unit tests: codec helpers ─────────────────────────────────────────────────

#[test]
fn nul_str_appends_zero() {
    let out = nul_str("hello");
    assert_eq!(out, b"hello\0");
}

#[test]
fn nul_str_empty() {
    assert_eq!(nul_str(""), b"\0");
}

#[test]
fn le_u64_reads_correctly() {
    let buf = 0x0102030405060708u64.to_le_bytes();
    assert_eq!(le_u64(&buf, 0), 0x0102030405060708);
}

#[test]
fn le_u64_at_offset() {
    let mut buf = vec![0u8; 16];
    buf[8..16].copy_from_slice(&42u64.to_le_bytes());
    assert_eq!(le_u64(&buf, 8), 42);
}

#[test]
fn le_u64_short_buffer_returns_zero() {
    let buf = [1u8; 4];
    assert_eq!(le_u64(&buf, 0), 0); // needs 8 bytes
}

#[test]
fn parse_nul_strings_basic() {
    let data = b"DCIM\0Books\0Podcasts\0";
    let out  = parse_nul_strings(data);
    assert_eq!(out, vec!["DCIM", "Books", "Podcasts"]);
}

#[test]
fn parse_nul_strings_filters_empty() {
    // Double-NUL or trailing NUL must not produce empty strings
    let data = b"foo\0\0bar\0";
    let out  = parse_nul_strings(data);
    assert_eq!(out, vec!["foo", "bar"]);
}

#[test]
fn parse_kv_pairs_basic() {
    let data = b"st_ifmt\0S_IFREG\0st_size\0123456\0";
    let map  = parse_kv_pairs(data);
    assert_eq!(map.get("st_ifmt").map(String::as_str), Some("S_IFREG"));
    assert_eq!(map.get("st_size").map(String::as_str), Some("123456"));
}

#[test]
fn parse_kv_pairs_odd_count_ignored() {
    // Trailing key without value should be silently dropped
    let data = b"st_ifmt\0S_IFDIR\0orphan\0";
    let map  = parse_kv_pairs(data);
    assert!(map.contains_key("st_ifmt"));
    assert!(!map.contains_key("orphan"));
}

// ── unit tests: FileInfo parsing ──────────────────────────────────────────────

#[test]
fn file_info_parses_regular_file() {
    let data = b"st_ifmt\0S_IFREG\0st_size\099\0st_mtime\01700000000000000000\0";
    let info = FileInfo::parse("photo.jpg", data);
    assert_eq!(info.name, "photo.jpg");
    assert!(matches!(info.file_type, FileType::Regular));
    assert_eq!(info.size, 99);
    assert_eq!(info.modified_nanos, 1700000000000000000);
    assert!(info.link_target.is_none());
}

#[test]
fn file_info_parses_directory() {
    let data = b"st_ifmt\0S_IFDIR\0st_size\00\0st_mtime\00\0";
    let info = FileInfo::parse("DCIM", data);
    assert!(matches!(info.file_type, FileType::Directory));
    assert_eq!(info.size, 0);
}

#[test]
fn file_info_parses_symlink_with_target() {
    let data = b"st_ifmt\0S_IFLNK\0st_size\00\0st_mtime\00\0LinkTarget\0/private/var/mobile/Media\0";
    let info = FileInfo::parse("Media", data);
    assert!(matches!(info.file_type, FileType::Symlink));
    assert_eq!(info.link_target.as_deref(), Some("/private/var/mobile/Media"));
}

#[test]
fn file_info_unknown_type() {
    let data = b"st_ifmt\0S_IFBLK\0st_size\00\0st_mtime\00\0";
    let info = FileInfo::parse("dev", data);
    assert!(matches!(info.file_type, FileType::Other));
}

// ── unit tests: AfcDeviceInfo parsing ────────────────────────────────────────

#[test]
fn device_info_parses_correctly() {
    let data = b"Model\0D421AP\0FSTotalBytes\016000000000\0FSFreeBytes\08000000000\0FSBlockSize\04096\0";
    let dev  = AfcDeviceInfo::parse(data);
    assert_eq!(dev.model, "D421AP");
    assert_eq!(dev.total_bytes, 16_000_000_000);
    assert_eq!(dev.free_bytes,   8_000_000_000);
    assert_eq!(dev.block_size,           4_096);
}

// ── unit tests: status names ──────────────────────────────────────────────────

#[test]
fn status_name_known_codes() {
    assert!(status_name(0).contains("success"));
    assert!(status_name(8).contains("not found"));
    assert!(status_name(16).contains("exists"));
}

#[test]
fn status_name_unknown_code() {
    let s = status_name(255);
    assert!(s.contains("255"));
}

// ── unit tests: wire format ───────────────────────────────────────────────────

/// Build a minimal AFC request into a Vec<u8> and verify the header fields.
#[test]
fn request_header_format() {
    use std::io::Cursor;

    // We need a MuxSocket to feed into AfcClient. MuxSocket::external wraps
    // any Read + Write, so we build a cursor pre-loaded with a synthetic
    // STATUS=success response and drain it after the write.
    let magic = b"CFA6LPAA";
    let mut response = Vec::new();
    // Header: magic + entire_length + this_length + packet_num + op(status)
    response.extend_from_slice(magic);                      // [0..8]   magic
    response.extend_from_slice(&48u64.to_le_bytes());       // [8..16]  entire_length = 40+8
    response.extend_from_slice(&48u64.to_le_bytes());       // [16..24] this_length
    response.extend_from_slice(&1u64.to_le_bytes());        // [24..32] packet_num
    response.extend_from_slice(&1u64.to_le_bytes());        // [32..40] op = STATUS
    response.extend_from_slice(&0u64.to_le_bytes());        // [40..48] status = SUCCESS

    struct MockStream {
        read:    Cursor<Vec<u8>>,
        written: Vec<u8>,
    }
    impl std::io::Read for MockStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> { self.read.read(buf) }
    }
    impl std::io::Write for MockStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    let mock   = MockStream { read: Cursor::new(response), written: Vec::new() };
    let stream = ios_rs::usbmux::MuxSocket::external(mock);
    let mut afc = AfcClient::from_stream(stream);

    // mkdir "/" should send a MAKE_DIR request (op = 9) and parse the SUCCESS status.
    afc.mkdir("/").expect("mkdir with mocked success response");
}

// ── integration tests: require a connected iOS device ────────────────────────
//
// Run with: cargo test --test afc -- --ignored
// or:       just test-device (if you add that recipe)

#[test]
#[ignore = "requires connected iOS device"]
fn integration_list_root() {
    setup();
    use ios_rs::usbmux::Connection;
    use ios_rs::lockdown::LockdownSession;

    let mut conn    = Connection::open().expect("usbmux connect");
    let devices     = conn.list_devices().expect("list devices");
    let device      = devices.into_iter().next().expect("no device connected");
    let mut session = LockdownSession::open_paired(device.device_id, &device.serial)
        .expect("lockdown session");
    let mut afc = AfcClient::connect(&mut session).expect("AFC connect");

    let entries = afc.list_dir("/").expect("list /");
    assert!(!entries.is_empty(), "root should have entries");
    // The media partition always has at least DCIM
    assert!(entries.iter().any(|e| e == "DCIM"),
            "DCIM not found in /: {entries:?}");
}

#[test]
#[ignore = "requires connected iOS device"]
fn integration_device_info_nonzero() {
    setup();
    use ios_rs::usbmux::Connection;
    use ios_rs::lockdown::LockdownSession;

    let mut conn    = Connection::open().expect("usbmux connect");
    let devices     = conn.list_devices().expect("list devices");
    let device      = devices.into_iter().next().expect("no device connected");
    let mut session = LockdownSession::open_paired(device.device_id, &device.serial)
        .expect("lockdown session");
    let mut afc = AfcClient::connect(&mut session).expect("AFC connect");

    let dev = afc.device_info().expect("device_info");
    assert!(!dev.model.is_empty(),  "model should not be empty");
    assert!(dev.total_bytes > 0,    "total_bytes should be > 0");
    assert!(dev.free_bytes > 0,     "free_bytes should be > 0");
    assert!(dev.block_size > 0,     "block_size should be > 0");
    assert!(dev.free_bytes <= dev.total_bytes);
}

#[test]
#[ignore = "requires connected iOS device"]
fn integration_mkdir_stat_remove() {
    setup();
    use ios_rs::usbmux::Connection;
    use ios_rs::lockdown::LockdownSession;

    let mut conn    = Connection::open().expect("usbmux connect");
    let devices     = conn.list_devices().expect("list devices");
    let device      = devices.into_iter().next().expect("no device connected");
    let mut session = LockdownSession::open_paired(device.device_id, &device.serial)
        .expect("lockdown session");
    let mut afc = AfcClient::connect(&mut session).expect("AFC connect");

    let dir = "/ios_rs_test_dir";

    afc.mkdir(dir).expect("mkdir");

    let info = afc.get_file_info(dir).expect("stat created dir");
    assert!(matches!(info.file_type, FileType::Directory));

    // Creating it again should be idempotent.
    afc.mkdir(dir).expect("mkdir again should succeed");

    afc.remove_path(dir).expect("remove dir");

    let err = afc.get_file_info(dir);
    assert!(err.is_err(), "stat after remove should fail");
}

#[test]
#[ignore = "requires connected iOS device"]
fn integration_write_read_remove() {
    setup();
    use ios_rs::usbmux::Connection;
    use ios_rs::lockdown::LockdownSession;

    let mut conn    = Connection::open().expect("usbmux connect");
    let devices     = conn.list_devices().expect("list devices");
    let device      = devices.into_iter().next().expect("no device connected");
    let mut session = LockdownSession::open_paired(device.device_id, &device.serial)
        .expect("lockdown session");
    let mut afc = AfcClient::connect(&mut session).expect("AFC connect");

    let path    = "/ios_rs_test_file.bin";
    let payload = b"hello from ios-rs afc test";

    afc.put_file(path, payload).expect("put_file");

    let data = afc.read_file(path).expect("read_file");
    assert_eq!(data, payload);

    afc.remove_path(path).expect("remove");
}

#[test]
#[ignore = "requires connected iOS device"]
fn integration_rename() {
    setup();
    use ios_rs::usbmux::Connection;
    use ios_rs::lockdown::LockdownSession;

    let mut conn    = Connection::open().expect("usbmux connect");
    let devices     = conn.list_devices().expect("list devices");
    let device      = devices.into_iter().next().expect("no device connected");
    let mut session = LockdownSession::open_paired(device.device_id, &device.serial)
        .expect("lockdown session");
    let mut afc = AfcClient::connect(&mut session).expect("AFC connect");

    // iOS AFC media-partition rename behaviour (observed on iOS 18.7.1):
    //
    // The `com.apple.afc` service does NOT implement rename as a true
    // move/overwrite.  Instead it always removes the source path:
    //   - dst exists:     src is deleted, dst keeps its original content
    //   - dst not exist:  src is deleted, dst is NOT created (file is lost)
    //
    // This appears to be an intentional iOS media-partition restriction.
    // True rename/move works on app-container paths via house_arrest, and on
    // jailbroken devices via com.apple.afc2.
    let src = "/ios_rs_rename_src.txt";
    let dst = "/ios_rs_rename_dst.txt";
    let dst_original = b"dst-original";

    afc.put_file(src, b"src-content").expect("put src");
    afc.put_file(dst, dst_original).expect("put dst");

    afc.rename(src, dst).expect("rename returns success");

    // src is always removed
    assert!(afc.get_file_info(src).is_err(), "src should be gone");

    // dst exists (because it was pre-created) but retains its own content
    let info = afc.get_file_info(dst).expect("dst should exist");
    assert!(matches!(info.file_type, FileType::Regular));
    let content = afc.read_file(dst).expect("read dst");
    assert_eq!(content, dst_original, "dst retains its original content (not moved from src)");

    afc.remove_path(dst).expect("cleanup");
}
