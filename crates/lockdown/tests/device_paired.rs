/// Integration tests requiring a paired iOS device.
///
/// Run with:
///   cargo test -p lockdown --test device_paired -- --nocapture
use lockdown::{LockdownSession, PairRecord};
use usbmux::Connection;

struct Device { id: u32, serial: String }

fn first_usb_device() -> Option<Device> {
    let mut conn = Connection::open().ok()?;
    conn.list_devices().ok()?
        .into_iter()
        .find(|d| d.connection_type == usbmux::ConnectionType::Usb)
        .map(|d| Device { id: d.device_id, serial: d.serial })
}

fn require_device() -> Device {
    first_usb_device().expect("no USB iOS device connected")
}

// ── pair record ───────────────────────────────────────────────────────────────

#[test]
fn paired_read_pair_record() {
    let dev    = require_device();
    let record = PairRecord::read_from_usbmuxd(&dev.serial)
        .expect("ReadPairRecord failed — is the device paired?");
    assert!(!record.host_id.is_empty(),    "HostID empty");
    assert!(!record.system_buid.is_empty(), "SystemBUID empty");
    assert!(!record.host_certificate.is_empty(), "HostCertificate empty");
    assert!(!record.host_private_key.is_empty(),  "HostPrivateKey empty");
    assert!(!record.root_certificate.is_empty(),  "RootCertificate empty");
    println!("HostID:     {}", record.host_id);
    println!("SystemBUID: {}", record.system_buid);
    println!("Cert bytes: {}", record.host_certificate.len());
    println!("Key  bytes: {}", record.host_private_key.len());
}

// ── start_session + TLS ───────────────────────────────────────────────────────

#[test]
fn paired_start_session() {
    let dev    = require_device();
    let record = PairRecord::read_from_usbmuxd(&dev.serial).expect("ReadPairRecord");

    let mut session = LockdownSession::connect(dev.id).expect("connect");
    let session_id  = session.start_session(&record).expect("StartSession");

    assert!(!session_id.is_empty(), "SessionID empty");
    println!("SessionID: {session_id}");
}

// ── get values only available over TLS ───────────────────────────────────────

#[test]
fn paired_get_serial_number() {
    let dev     = require_device();
    let record  = PairRecord::read_from_usbmuxd(&dev.serial).expect("ReadPairRecord");
    let mut session = LockdownSession::open_paired(dev.id, &dev.serial).expect("open_paired");

    let serial = session.get_value(None, "SerialNumber").expect("GetValue SerialNumber");
    match &serial {
        plist::Value::String(s) => {
            assert!(!s.is_empty(), "SerialNumber empty");
            println!("SerialNumber: {s}");
        }
        _ => panic!("SerialNumber not a string: {serial:?}"),
    }
}

#[test]
fn paired_get_udid() {
    let dev     = require_device();
    let _record = PairRecord::read_from_usbmuxd(&dev.serial).expect("ReadPairRecord");
    let mut session = LockdownSession::open_paired(dev.id, &dev.serial).expect("open_paired");

    let udid = session.get_value(None, "UniqueDeviceID").expect("GetValue UniqueDeviceID");
    match &udid {
        plist::Value::String(s) => {
            assert!(!s.is_empty(), "UniqueDeviceID empty");
            println!("UniqueDeviceID: {s}");
        }
        _ => panic!("UniqueDeviceID not a string: {udid:?}"),
    }
}

#[test]
fn paired_get_all_values_has_serial() {
    let dev     = require_device();
    let mut session = LockdownSession::open_paired(dev.id, &dev.serial).expect("open_paired");

    let info = session.get_all_values().expect("get_all_values");
    println!("Name:         {}", info.device_name);
    println!("Product:      {}", info.product_type);
    println!("iOS version:  {}", info.product_version);
    println!("SerialNumber: {}", info.serial_number);
    println!("UDID:         {}", info.unique_device_id);
    println!("Hardware:     {}", info.hardware_model);
    // These should now be populated over TLS
    assert!(!info.serial_number.is_empty(),   "SerialNumber still empty after StartSession");
    assert!(!info.unique_device_id.is_empty(), "UniqueDeviceID still empty after StartSession");
}

// ── start_service over TLS ────────────────────────────────────────────────────

#[test]
fn paired_start_service_heartbeat() {
    let dev     = require_device();
    let mut session = LockdownSession::open_paired(dev.id, &dev.serial).expect("open_paired");

    let svc = session.start_service("com.apple.mobile.heartbeat")
        .expect("StartService heartbeat");
    assert!(svc.port > 0, "heartbeat port 0");
    println!("heartbeat port={} ssl={}", svc.port, svc.enable_service_ssl);
}

#[test]
fn paired_list_services() {
    let dev     = require_device();
    let mut session = LockdownSession::open_paired(dev.id, &dev.serial).expect("open_paired");

    // list_services is best-effort; not all iOS versions expose the catalogue
    let mut services = session.list_services().expect("list_services should not error");
    services.sort();
    if services.is_empty() {
        println!("services: catalogue not exposed by this iOS version (ok)");
    } else {
        println!("services ({}):", services.len());
        for s in &services { println!("  {s}"); }
        assert!(services.iter().any(|s| s.contains("heartbeat")),
            "heartbeat missing from services list: {services:?}");
    }
}

// ── multiple paired sessions ──────────────────────────────────────────────────

#[test]
fn paired_multiple_sessions() {
    let dev = require_device();
    for i in 0..3 {
        let mut session = LockdownSession::open_paired(dev.id, &dev.serial)
            .expect("open_paired");
        let info = session.get_all_values().expect("get_all_values");
        assert!(!info.serial_number.is_empty(), "session {i}: empty SerialNumber");
        println!("session {i}: serial={}", info.serial_number);
    }
}
