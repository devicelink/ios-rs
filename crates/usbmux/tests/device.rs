/// Integration tests against a real device via usbmuxd.
///
/// Run with:
///   cargo test -p usbmux --test device
///
/// The tests skip gracefully when no device is connected.
use usbmux::{Connection, ConnectionType, Event};

fn open() -> Option<Connection> {
    Connection::open().ok()
}

fn require_conn() -> Connection {
    open().expect("could not connect to usbmuxd — is it running?")
}

// ── list_devices ─────────────────────────────────────────────────────────────

#[test]
fn device_list_devices_not_empty() {
    let mut conn    = require_conn();
    let devices     = conn.list_devices().expect("list_devices failed");
    assert!(!devices.is_empty(), "no devices connected — plug in an iOS device");
    println!("connected devices ({}):", devices.len());
    for d in &devices {
        println!("  {} [{:?}] product={:#06x}", d.serial, d.connection_type, d.product_id);
    }
}

#[test]
fn device_list_devices_has_udids() {
    let mut conn = require_conn();
    let devices  = conn.list_devices().expect("list_devices");
    for d in &devices {
        assert!(!d.serial.is_empty(), "device has empty serial/UDID");
        // UDIDs are either 40 hex chars (old) or 25-char format like 00008030-000E04D62E8B802E
        assert!(d.serial.len() >= 24, "UDID too short: {}", d.serial);
    }
}

#[test]
fn device_list_devices_connection_types_known() {
    let mut conn = require_conn();
    let devices  = conn.list_devices().expect("list_devices");
    for d in &devices {
        match d.connection_type {
            ConnectionType::Usb | ConnectionType::Network => {}
            ConnectionType::Unknown(ref s) => {
                panic!("unexpected connection type {:?} for {}", s, d.serial);
            }
        }
    }
}

// ── read_buid ─────────────────────────────────────────────────────────────────

#[test]
fn device_read_buid_format() {
    let mut conn = require_conn();
    let buid     = conn.read_buid().expect("read_buid failed");
    assert!(!buid.is_empty(), "BUID is empty");
    // BUIDs look like "DEADBEEF-CAFE-CAFE-CAFE-DEADBEEFCAFE"
    assert!(buid.contains('-'), "BUID doesn't look like UUID: {buid}");
    println!("BUID: {buid}");
}

// ── multiple sequential list calls ───────────────────────────────────────────

#[test]
fn device_list_devices_idempotent() {
    // Each call opens a fresh connection; both should return the same device count.
    let mut c1 = require_conn();
    let mut c2 = require_conn();
    let d1     = c1.list_devices().expect("first list");
    let d2     = c2.list_devices().expect("second list");
    assert_eq!(d1.len(), d2.len(), "device count changed between two successive calls");
}

// ── open_tunnel to lockdownd ──────────────────────────────────────────────────

#[test]
fn device_open_tunnel_to_lockdownd() {
    let mut conn    = require_conn();
    let devices     = conn.list_devices().expect("list_devices");
    let Some(dev)   = devices.first() else { return };  // skip if no device

    let mut conn2   = require_conn();
    let mut tunnel  = conn2.open_tunnel(dev.device_id, 62078)
        .expect("open_tunnel to lockdownd (port 62078)");

    // Send a minimal lockdownd QueryType request and expect a plist response.
    let req = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
        \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\
        <plist version=\"1.0\"><dict>\
        <key>Request</key><string>QueryType</string>\
        </dict></plist>";

    use std::io::{Read, Write};
    tunnel.write_all(&(req.len() as u32).to_be_bytes()).unwrap();
    tunnel.write_all(req).unwrap();
    tunnel.flush().unwrap();

    let mut len_buf = [0u8; 4];
    tunnel.read_exact(&mut len_buf).unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    assert!(len > 0 && len < 65536, "implausible response length {len}");

    let mut body = vec![0u8; len];
    tunnel.read_exact(&mut body).unwrap();
    let s = std::str::from_utf8(&body).unwrap_or("<binary>");
    assert!(s.contains("com.apple.mobile.lockdown"), "unexpected QueryType response: {s}");
    println!("QueryType response ({len} bytes): {s}");
}

// ── listen for events ─────────────────────────────────────────────────────────

#[test]
fn device_listen_returns_initial_attached_events() {
    use std::time::{Duration, Instant};

    let mut conn = require_conn();
    let device_count = conn.list_devices().expect("list").len();
    if device_count == 0 { return; }

    let conn2 = require_conn();
    let mut listener = conn2.listen().expect("listen failed");

    // The real usbmuxd sends Attached events for all currently-connected
    // devices immediately after a Listen request.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = 0usize;
    while Instant::now() < deadline && seen < device_count {
        match listener.next() {
            Ok(Event::DeviceAttached(d)) => {
                println!("Attached: {} [{:?}]", d.serial, d.connection_type);
                seen += 1;
            }
            Ok(Event::DeviceDetached { device_id }) => {
                println!("Detached: device_id={device_id}");
            }
            Ok(_) => {}
            Err(e) => { eprintln!("listen error: {e}"); break; }
        }
    }
    assert_eq!(seen, device_count,
        "expected {device_count} Attached events, got {seen}");
}
