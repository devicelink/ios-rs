use std::io::Read;
use std::net::TcpStream;

use crate::framing;
use crate::server::SimDevice;

/// Handle one client connection. Returns normally when the client disconnects.
pub fn handle(mut stream: TcpStream, devices: &[SimDevice]) {
    loop {
        let (tag, payload) = match framing::read_frame(&mut stream) {
            Ok(f)  => f,
            Err(_) => return, // client disconnected
        };

        let msg_type = match framing::message_type(&payload) {
            Some(t) => t,
            None    => return,
        };

        match msg_type.as_str() {
            "ListDevices" => handle_list_devices(&mut stream, tag, devices),
            "Listen"      => handle_listen(&mut stream, tag, devices),
            "Connect"     => handle_connect(&mut stream, tag, &payload, devices),
            "ReadBUID"    => handle_read_buid(&mut stream, tag),
            _             => {
                // Unknown command — reply with BadCommand
                let resp = framing::result_plist(1);
                let _ = framing::write_frame(&mut stream, tag, &resp);
                return;
            }
        }

        // After Connect succeeds, the socket becomes a transparent tunnel —
        // the handler for Connect either loops internally or returns here.
        if msg_type == "Connect" { return; }
    }
}

fn handle_list_devices(stream: &mut TcpStream, tag: u32, devices: &[SimDevice]) {
    let entries: Vec<plist::Value> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let mut props = plist::Dictionary::new();
            props.insert("SerialNumber".into(),   plist::Value::String(d.serial.clone()));
            props.insert("ConnectionType".into(), plist::Value::String(d.connection_type.clone()));
            props.insert("ProductID".into(),      plist::Value::Integer((d.product_id as i64).into()));
            props.insert("LocationID".into(),     plist::Value::Integer(0.into()));

            let mut entry = plist::Dictionary::new();
            entry.insert("DeviceID".into(),    plist::Value::Integer(((i + 1) as i64).into()));
            entry.insert("MessageType".into(), plist::Value::String("Attached".into()));
            entry.insert("Properties".into(),  plist::Value::Dictionary(props));
            plist::Value::Dictionary(entry)
        })
        .collect();

    let resp = framing::device_list_plist(&entries);
    let _ = framing::write_frame(stream, tag, &resp);
}

fn handle_listen(stream: &mut TcpStream, tag: u32, devices: &[SimDevice]) {
    // Acknowledge the Listen request
    let ok = framing::result_plist(0);
    if framing::write_frame(stream, tag, &ok).is_err() { return; }

    // Immediately emit Attached events for all currently-connected devices
    for (i, d) in devices.iter().enumerate() {
        let device_id = (i + 1) as u32;
        let ev = framing::attached_plist(device_id, &d.serial, &d.connection_type, d.product_id);
        // Use tag=0 for unsolicited events
        if framing::write_frame(stream, 0, &ev).is_err() { return; }
    }

    // Keep the connection open and drain any incoming data until the client closes
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_)          => {} // ignore; listen connection is read-only
        }
    }
}

fn handle_connect(stream: &mut TcpStream, tag: u32, payload: &[u8], devices: &[SimDevice]) {
    let (device_id, port) = match framing::parse_connect(payload) {
        Some(p) => p,
        None    => {
            let _ = framing::write_frame(stream, tag, &framing::result_plist(1));
            return;
        }
    };

    // Find the device
    let dev_idx = (device_id as usize).saturating_sub(1);
    let device  = match devices.get(dev_idx) {
        Some(d) => d,
        None    => {
            let _ = framing::write_frame(stream, tag, &framing::result_plist(2)); // BadDevice
            return;
        }
    };

    // Find the service handler for this port
    let service_fn = device.services
        .iter()
        .find(|(p, _)| *p == port)
        .map(|(_, f)| f.clone());

    let service_fn = match service_fn {
        Some(f) => f,
        None    => {
            let _ = framing::write_frame(stream, tag, &framing::result_plist(3)); // ConnRefused
            return;
        }
    };

    // Send Result:OK — socket is now a transparent tunnel
    if framing::write_frame(stream, tag, &framing::result_plist(0)).is_err() { return; }

    // Hand the socket to the service handler
    service_fn(stream);
}

fn handle_read_buid(stream: &mut TcpStream, tag: u32) {
    let mut d = plist::Dictionary::new();
    d.insert("BUID".into(), plist::Value::String("DEADBEEF-0000-0000-0000-CAFECAFECAFE".into()));
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, &plist::Value::Dictionary(d)).unwrap();
    let _ = framing::write_frame(stream, tag, &buf);
}
