use std::io::{Read, Write};

use ios_rs::usbmux::sim::{SimDevice, UsbmuxSim};
use ios_rs::usbmux::Connection;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Plist framing for service responses (u32-BE length prefix).
fn send_plist_response(stream: &mut std::net::TcpStream, plist: &[u8]) {
    let len = plist.len() as u32;
    stream.write_all(&len.to_be_bytes()).unwrap();
    stream.write_all(plist).unwrap();
    stream.flush().unwrap();
}

// ── list_devices ─────────────────────────────────────────────────────────────

#[test]
fn list_devices_empty() {
    let sim = UsbmuxSim::start(vec![]);
    let mut conn = Connection::open_at(sim.addr()).unwrap();
    let devices = conn.list_devices().unwrap();
    assert!(devices.is_empty());
}

#[test]
fn list_devices_one_usb() {
    let sim = UsbmuxSim::start(vec![SimDevice::usb("AABBCC112233")]);
    let mut conn = Connection::open_at(sim.addr()).unwrap();
    let devices = conn.list_devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].serial, "AABBCC112233");
    assert_eq!(
        devices[0].connection_type,
        ios_rs::usbmux::ConnectionType::Usb
    );
    assert_eq!(devices[0].product_id, 0x12a8);
}

#[test]
fn list_devices_mixed() {
    let sim = UsbmuxSim::start(vec![
        SimDevice::usb("USB_DEVICE_001"),
        SimDevice::network("NET_DEVICE_002"),
    ]);
    let mut conn = Connection::open_at(sim.addr()).unwrap();
    let devices = conn.list_devices().unwrap();
    assert_eq!(devices.len(), 2);

    let usb = devices
        .iter()
        .find(|d| d.serial == "USB_DEVICE_001")
        .unwrap();
    let net = devices
        .iter()
        .find(|d| d.serial == "NET_DEVICE_002")
        .unwrap();
    assert_eq!(usb.connection_type, ios_rs::usbmux::ConnectionType::Usb);
    assert_eq!(net.connection_type, ios_rs::usbmux::ConnectionType::Network);
}

#[test]
fn multiple_sequential_list_requests() {
    // Each `list_devices` opens a new connection (Connection is consumed).
    let sim = UsbmuxSim::start(vec![SimDevice::usb("DEV1"), SimDevice::usb("DEV2")]);
    for _ in 0..3 {
        let mut conn = Connection::open_at(sim.addr()).unwrap();
        let devices = conn.list_devices().unwrap();
        assert_eq!(devices.len(), 2);
    }
}

// ── listen ────────────────────────────────────────────────────────────────────

#[test]
fn listen_receives_initial_attach_events() {
    let sim = UsbmuxSim::start(vec![SimDevice::usb("LISTEN_A"), SimDevice::usb("LISTEN_B")]);
    let conn = Connection::open_at(sim.addr()).unwrap();
    let mut listener = conn.listen().unwrap();

    // The sim immediately emits Attached for each device on Listen
    let ev1 = listener.next().unwrap();
    let ev2 = listener.next().unwrap();

    let mut serials: Vec<String> = vec![
        match ev1 {
            ios_rs::usbmux::Event::DeviceAttached(d) => d.serial,
            _ => panic!("not attached"),
        },
        match ev2 {
            ios_rs::usbmux::Event::DeviceAttached(d) => d.serial,
            _ => panic!("not attached"),
        },
    ];
    serials.sort();
    assert_eq!(serials, ["LISTEN_A", "LISTEN_B"]);
}

// ── open_tunnel ───────────────────────────────────────────────────────────────

#[test]
fn connect_to_unknown_port_returns_error() {
    let sim = UsbmuxSim::start(vec![SimDevice::usb("DEV_NO_SERVICES")]);
    let conn = Connection::open_at(sim.addr()).unwrap();
    let err = conn.open_tunnel(1, 62078);
    assert!(
        err.is_err(),
        "expected error connecting to unregistered port"
    );
}

#[test]
fn connect_to_bad_device_returns_error() {
    let sim = UsbmuxSim::start(vec![SimDevice::usb("SOLO")]);
    let conn = Connection::open_at(sim.addr()).unwrap();
    // device_id=999 doesn't exist
    let err = conn.open_tunnel(999, 62078);
    assert!(err.is_err());
}

#[test]
fn open_tunnel_and_echo_service() {
    // Minimal echo service: reads 4 bytes, writes them back.
    let sim = UsbmuxSim::start(vec![SimDevice::usb("ECHO_DEV").with_service(
        9999,
        |stream| {
            let mut buf = [0u8; 4];
            if stream.read_exact(&mut buf).is_ok() {
                let _ = stream.write_all(&buf);
                let _ = stream.flush();
            }
        },
    )]);

    let conn = Connection::open_at(sim.addr()).unwrap();
    let mut tunnel = conn.open_tunnel(1, 9999).unwrap();

    let sent = b"PING";
    tunnel.write_all(sent).unwrap();
    tunnel.flush().unwrap();

    let mut reply = [0u8; 4];
    tunnel.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, sent);
}

#[test]
fn open_tunnel_lockdown_style_service() {
    // Simulates a minimal lockdownd-style service:
    //   - reads a 4-byte BE plist length + plist
    //   - writes back a fixed GetValue response
    let response_plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Request</key><string>GetValue</string>
  <key>Value</key><string>iPhone17,2</string>
</dict></plist>"#;

    let resp = response_plist.to_vec();
    let sim = UsbmuxSim::start(vec![SimDevice::usb("LOCKDOWN_DEV").with_service(
        62078,
        move |stream| {
            // Drain the incoming request (length-prefixed)
            let mut len_buf = [0u8; 4];
            if stream.read_exact(&mut len_buf).is_err() {
                return;
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut req = vec![0u8; len];
            let _ = stream.read_exact(&mut req);

            // Send response
            send_plist_response(stream, &resp);
        },
    )]);

    let conn = Connection::open_at(sim.addr()).unwrap();
    let mut tunnel = conn.open_tunnel(1, 62078).unwrap();

    // Send a fake GetValue request
    let req_plist = b"<plist><dict><key>Request</key><string>GetValue</string></dict></plist>";
    let len = req_plist.len() as u32;
    tunnel.write_all(&len.to_be_bytes()).unwrap();
    tunnel.write_all(req_plist).unwrap();
    tunnel.flush().unwrap();

    // Read back the response
    let mut resp_len_buf = [0u8; 4];
    tunnel.read_exact(&mut resp_len_buf).unwrap();
    let resp_len = u32::from_be_bytes(resp_len_buf) as usize;
    let mut resp_body = vec![0u8; resp_len];
    tunnel.read_exact(&mut resp_body).unwrap();

    let resp_str = std::str::from_utf8(&resp_body).unwrap();
    assert!(
        resp_str.contains("iPhone17,2"),
        "unexpected response: {resp_str}"
    );
}

// ── read_buid ────────────────────────────────────────────────────────────────

#[test]
fn read_buid_returns_string() {
    let sim = UsbmuxSim::start(vec![]);
    let mut conn = Connection::open_at(sim.addr()).unwrap();
    let buid = conn.read_buid().unwrap();
    assert!(!buid.is_empty(), "BUID should not be empty");
    assert!(buid.contains('-'), "BUID looks like a UUID: {buid}");
}

// ── concurrent connections ────────────────────────────────────────────────────

#[test]
fn concurrent_connections_see_same_device_list() {
    use std::thread;

    let sim = std::sync::Arc::new(UsbmuxSim::start(vec![
        SimDevice::usb("CONCURRENT_A"),
        SimDevice::usb("CONCURRENT_B"),
    ]));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let addr = sim.addr();
            thread::spawn(move || {
                let mut conn = Connection::open_at(addr).unwrap();
                conn.list_devices().unwrap()
            })
        })
        .collect();

    for h in handles {
        let devices = h.join().unwrap();
        assert_eq!(devices.len(), 2);
    }
}
