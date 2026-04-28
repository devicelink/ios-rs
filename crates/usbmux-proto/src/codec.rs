use std::collections::VecDeque;

use crate::plist_msg::{self, BaseRequest, ConnectRequest, PairRecordRequest, SavePairRecordRequest};
use crate::types::{ConnectionType, Device, Event, ResultCode};

const HEADER_LEN: usize = 16;
const VERSION_PLIST: u32 = 1;
const MSGTYPE_PLIST: u32 = 8;

pub struct Codec {
    rx:         Vec<u8>,
    tx:         VecDeque<Vec<u8>>,
    events:     VecDeque<Event>,
    next_tag:   u32,
    /// tags that are Connect requests — on OK result we emit Connected, not RequestFailed
    pending_connects: VecDeque<u32>,
}

impl Default for Codec {
    fn default() -> Self { Self::new() }
}

impl Codec {
    pub fn new() -> Self {
        Codec {
            rx:               Vec::new(),
            tx:               VecDeque::new(),
            events:           VecDeque::new(),
            next_tag:         1,
            pending_connects: VecDeque::new(),
        }
    }

    // ── request builders ────────────────────────────────────────────────────

    pub fn list_devices(&mut self) -> u32 {
        let tag = self.alloc_tag();
        let body = plist_msg::encode(&BaseRequest::new("ListDevices")).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    pub fn listen(&mut self) -> u32 {
        let tag = self.alloc_tag();
        let body = plist_msg::encode(&BaseRequest::new("Listen")).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    pub fn read_buid(&mut self) -> u32 {
        let tag = self.alloc_tag();
        // ReadBUID uses minimal envelope (no ClientVersionString required)
        let body = plist_msg::encode(&BaseRequest::new("ReadBUID")).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    pub fn read_pair_record(&mut self, udid: &str) -> u32 {
        let tag = self.alloc_tag();
        let req = PairRecordRequest {
            message_type:          "ReadPairRecord".into(),
            client_version_string: "devicelink-0.1.0".into(),
            prog_name:             "devicelink".into(),
            lib_usbmux_version:    3,
            pair_record_id:        udid.to_string(),
        };
        let body = plist_msg::encode(&req).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    pub fn save_pair_record(&mut self, udid: &str, record: Vec<u8>) -> u32 {
        let tag = self.alloc_tag();
        let req = SavePairRecordRequest {
            message_type:          "SavePairRecord".into(),
            client_version_string: "devicelink-0.1.0".into(),
            prog_name:             "devicelink".into(),
            lib_usbmux_version:    3,
            pair_record_id:        udid.to_string(),
            pair_record_data:      record,
        };
        let body = plist_msg::encode(&req).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    /// port must be in host byte order; we swap to network order here
    pub fn connect(&mut self, device_id: u32, port: u16) -> u32 {
        let tag = self.alloc_tag();
        self.pending_connects.push_back(tag);
        let req = ConnectRequest {
            message_type:          "Connect".into(),
            client_version_string: "devicelink-0.1.0".into(),
            prog_name:             "devicelink".into(),
            lib_usbmux_version:    3,
            device_id,
            port_number:           port.to_be(),  // network byte order
        };
        let body = plist_msg::encode(&req).unwrap();
        self.enqueue_frame(&body, tag);
        tag
    }

    // ── I/O interface ────────────────────────────────────────────────────────

    /// Feed bytes received from the socket into the codec.
    pub fn push_data(&mut self, data: &[u8]) {
        self.rx.extend_from_slice(data);
        self.process_rx();
    }

    /// Pull the next frame that should be written to the socket, if any.
    pub fn poll_write(&mut self) -> Option<Vec<u8>> {
        self.tx.pop_front()
    }

    /// Pull the next decoded protocol event, if any.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn alloc_tag(&mut self) -> u32 {
        let t = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1).max(1);
        t
    }

    fn enqueue_frame(&mut self, body: &[u8], tag: u32) {
        let total = (HEADER_LEN + body.len()) as u32;
        let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
        frame.extend_from_slice(&total.to_le_bytes());
        frame.extend_from_slice(&VERSION_PLIST.to_le_bytes());
        frame.extend_from_slice(&MSGTYPE_PLIST.to_le_bytes());
        frame.extend_from_slice(&tag.to_le_bytes());
        frame.extend_from_slice(body);
        self.tx.push_back(frame);
    }

    fn process_rx(&mut self) {
        loop {
            if self.rx.len() < HEADER_LEN { return; }

            let total = u32::from_le_bytes(self.rx[0..4].try_into().unwrap()) as usize;
            if total < HEADER_LEN { return; }
            if self.rx.len() < total { return; }

            let tag      = u32::from_le_bytes(self.rx[12..16].try_into().unwrap());
            let payload  = self.rx[HEADER_LEN..total].to_vec();
            self.rx.drain(0..total);

            if let Some(event) = self.decode_frame(tag, &payload) {
                self.events.push_back(event);
            }
        }
    }

    fn decode_frame(&mut self, tag: u32, payload: &[u8]) -> Option<Event> {
        let env = match plist_msg::decode(payload) {
            Ok(e)  => e,
            Err(_) => return None,
        };

        let msg_type = env.message_type.as_deref().unwrap_or("");

        match msg_type {
            "Result" => {
                let code = ResultCode::from(env.number.unwrap_or(u32::MAX));
                let is_connect = self.pending_connects.iter().any(|&t| t == tag);
                if is_connect {
                    self.pending_connects.retain(|&t| t != tag);
                    if code == ResultCode::Ok {
                        return Some(Event::Connected { tag });
                    }
                }
                if code == ResultCode::Ok {
                    None  // plain OK with no meaningful payload
                } else {
                    Some(Event::RequestFailed { tag, code })
                }
            }

            "" => {
                // ListDevices response has no MessageType at top level
                if let Some(list) = env.device_list {
                    let devices = list.into_iter().map(device_from_entry).collect();
                    return Some(Event::DeviceList(devices));
                }
                // ReadBUID response
                if let Some(buid) = env.buid {
                    return Some(Event::Buid(buid));
                }
                // ReadPairRecord response
                if let Some(data) = env.pair_record_data {
                    return Some(Event::PairRecord {
                        udid:   String::new(),
                        record: data.into_vec(),
                    });
                }
                None
            }

            "Attached" => {
                let props = env.properties?;
                Some(Event::DeviceAttached(Device {
                    device_id:       env.device_id?,
                    serial:          props.serial_number.unwrap_or_default(),
                    connection_type: ConnectionType::from(
                        props.connection_type.as_deref().unwrap_or(""),
                    ),
                    product_id:      props.product_id.unwrap_or(0),
                    location_id:     props.location_id.unwrap_or(0),
                }))
            }

            "Detached" => Some(Event::DeviceDetached {
                device_id: env.device_id?,
            }),

            _ => None,
        }
    }
}

fn device_from_entry(e: plist_msg::DeviceEntry) -> Device {
    Device {
        device_id:       e.device_id,
        serial:          e.properties.serial_number.unwrap_or_default(),
        connection_type: ConnectionType::from(
            e.properties.connection_type.as_deref().unwrap_or(""),
        ),
        product_id:      e.properties.product_id.unwrap_or(0),
        location_id:     e.properties.location_id.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_list_devices() -> (Vec<u8>, Vec<u8>) {
        // Build a fake ListDevices response that mimics what usbmuxd sends
        let response_plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>DeviceList</key>
    <array>
        <dict>
            <key>DeviceID</key>
            <integer>5</integer>
            <key>MessageType</key>
            <string>Attached</string>
            <key>Properties</key>
            <dict>
                <key>SerialNumber</key>
                <string>ABC123</string>
                <key>ConnectionType</key>
                <string>USB</string>
                <key>ProductID</key>
                <integer>4776</integer>
                <key>LocationID</key>
                <integer>336592896</integer>
            </dict>
        </dict>
    </array>
</dict>
</plist>"#;

        let total = (HEADER_LEN + response_plist.len()) as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&total.to_le_bytes());
        frame.extend_from_slice(&VERSION_PLIST.to_le_bytes());
        frame.extend_from_slice(&MSGTYPE_PLIST.to_le_bytes());
        frame.extend_from_slice(&1u32.to_le_bytes()); // tag=1
        frame.extend_from_slice(response_plist);
        (frame, response_plist.to_vec())
    }

    #[test]
    fn list_devices_encodes_request() {
        let mut codec = Codec::new();
        let tag = codec.list_devices();
        assert_eq!(tag, 1);
        let frame = codec.poll_write().unwrap();
        // Header: total_len, version=1, type=8, tag=1
        assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), 8);
        assert_eq!(u32::from_le_bytes(frame[12..16].try_into().unwrap()), 1);
        assert!(frame.len() > HEADER_LEN);
    }

    #[test]
    fn list_devices_decodes_response() {
        let mut codec = Codec::new();
        let _tag = codec.list_devices();
        let _ = codec.poll_write();

        let (frame, _) = round_trip_list_devices();
        codec.push_data(&frame);

        match codec.poll_event().unwrap() {
            Event::DeviceList(devices) => {
                assert_eq!(devices.len(), 1);
                assert_eq!(devices[0].serial, "ABC123");
                assert_eq!(devices[0].device_id, 5);
                assert_eq!(devices[0].connection_type, ConnectionType::Usb);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn partial_frame_buffered() {
        let mut codec = Codec::new();
        let _tag = codec.list_devices();
        let _ = codec.poll_write();

        let (frame, _) = round_trip_list_devices();
        // Feed in two halves
        let mid = frame.len() / 2;
        codec.push_data(&frame[..mid]);
        assert!(codec.poll_event().is_none());
        codec.push_data(&frame[mid..]);
        assert!(codec.poll_event().is_some());
    }

    #[test]
    fn connect_emits_connected_on_ok() {
        let result_plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>MessageType</key><string>Result</string>
    <key>Number</key><integer>0</integer>
</dict></plist>"#;

        let mut codec = Codec::new();
        let tag = codec.connect(5, 62078);
        let _ = codec.poll_write();

        let total = (HEADER_LEN + result_plist.len()) as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&total.to_le_bytes());
        frame.extend_from_slice(&VERSION_PLIST.to_le_bytes());
        frame.extend_from_slice(&MSGTYPE_PLIST.to_le_bytes());
        frame.extend_from_slice(&tag.to_le_bytes());
        frame.extend_from_slice(result_plist);

        codec.push_data(&frame);
        match codec.poll_event().unwrap() {
            Event::Connected { tag: t } => assert_eq!(t, tag),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
