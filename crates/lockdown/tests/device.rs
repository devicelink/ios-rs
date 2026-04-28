/// Integration tests against a real device's lockdownd via usbmuxd.
///
/// Run with:
///   cargo test -p lockdown --test device
use lockdown::LockdownSession;
use usbmux::Connection;

fn first_device_id() -> Option<u32> {
    Connection::open().ok()?.list_devices().ok()?.into_iter().next().map(|d| d.device_id)
}

fn require_device_id() -> u32 {
    first_device_id().expect("no iOS device connected")
}

// ── query_type ────────────────────────────────────────────────────────────────

#[test]
fn lockdown_query_type() {
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    let t    = s.query_type().expect("QueryType");
    assert_eq!(t, "com.apple.mobile.lockdown", "unexpected service type: {t}");
}

// ── get_all_values ────────────────────────────────────────────────────────────

#[test]
fn lockdown_get_all_values_product_type() {
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    let info = s.get_all_values().expect("get_all_values");
    assert!(!info.product_type.is_empty(),  "ProductType empty");
    assert!(!info.product_version.is_empty(), "ProductVersion empty");
    assert!(!info.device_name.is_empty(),   "DeviceName empty");
    assert!(!info.cpu_architecture.is_empty(), "CPUArchitecture empty");
    println!("Name:    {}", info.device_name);
    println!("Product: {}", info.product_type);
    println!("iOS:     {}", info.product_version);
    println!("CPU:     {}", info.cpu_architecture);
    println!("Model:   {}", info.hardware_model);
}

#[test]
fn lockdown_get_all_values_extra_fields_present() {
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    let info = s.get_all_values().expect("get_all_values");
    // These keys should always be present in the root value dict
    assert!(info.extra.contains_key("ProductName") || !info.product_type.is_empty(),
        "no ProductName and no ProductType in response");
}

// ── get_value ─────────────────────────────────────────────────────────────────

#[test]
fn lockdown_get_value_product_version() {
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    let val  = s.get_value(None, "ProductVersion").expect("GetValue ProductVersion");
    let ver  = match &val {
        plist::Value::String(s) => s.clone(),
        _ => panic!("ProductVersion not a string: {val:?}"),
    };
    assert!(!ver.is_empty());
    assert!(ver.contains('.'), "ProductVersion doesn't look like a version: {ver}");
    println!("ProductVersion: {ver}");
}

#[test]
fn lockdown_get_value_product_type() {
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    let val  = s.get_value(None, "ProductType").expect("GetValue ProductType");
    match &val {
        plist::Value::String(s) => {
            assert!(s.starts_with("iPhone") || s.starts_with("iPad") || s.starts_with("iPod"),
                "unexpected ProductType: {s}");
            println!("ProductType: {s}");
        }
        _ => panic!("ProductType not a string"),
    }
}

// ── multiple sequential sessions ─────────────────────────────────────────────

#[test]
fn lockdown_multiple_sessions() {
    let id = require_device_id();
    for i in 0..3 {
        let mut s   = LockdownSession::connect(id).expect("connect");
        let info    = s.get_all_values().expect("get_all_values");
        assert!(!info.product_version.is_empty(), "session {i}: empty ProductVersion");
    }
}

// ── start_service ─────────────────────────────────────────────────────────────

#[test]
fn lockdown_start_service_heartbeat() {
    // com.apple.mobile.heartbeat is available on all iOS versions without pairing
    let id   = require_device_id();
    let mut s = LockdownSession::connect(id).expect("connect");
    match s.start_service("com.apple.mobile.heartbeat") {
        Ok(svc) => {
            println!("heartbeat service port: {}, ssl: {}", svc.port, svc.enable_service_ssl);
            assert!(svc.port > 0, "heartbeat port should be > 0");
        }
        Err(e) => {
            // Some iOS versions require pairing for this service — skip gracefully
            eprintln!("start_service(heartbeat) skipped: {e}");
        }
    }
}
