use serde::{Deserialize, Serialize};

// ── outbound request bodies ──────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BaseRequest {
    pub message_type: String,
    #[serde(rename = "ClientVersionString")]
    pub client_version_string: String,
    pub prog_name: String,
    #[serde(rename = "kLibUSBMuxVersion")]
    pub lib_usbmux_version: u32,
}

impl BaseRequest {
    pub fn new(message_type: &str) -> Self {
        BaseRequest {
            message_type: message_type.into(),
            client_version_string: "devicelink-0.1.0".into(),
            prog_name: "devicelink".into(),
            lib_usbmux_version: 3,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ConnectRequest {
    pub message_type: String,
    #[serde(rename = "ClientVersionString")]
    pub client_version_string: String,
    pub prog_name: String,
    #[serde(rename = "kLibUSBMuxVersion")]
    pub lib_usbmux_version: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: u32,
    pub port_number: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PairRecordRequest {
    pub message_type: String,
    #[serde(rename = "ClientVersionString")]
    pub client_version_string: String,
    pub prog_name: String,
    #[serde(rename = "kLibUSBMuxVersion")]
    pub lib_usbmux_version: u32,
    #[serde(rename = "PairRecordID")]
    pub pair_record_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SavePairRecordRequest {
    pub message_type: String,
    #[serde(rename = "ClientVersionString")]
    pub client_version_string: String,
    pub prog_name: String,
    #[serde(rename = "kLibUSBMuxVersion")]
    pub lib_usbmux_version: u32,
    #[serde(rename = "PairRecordID")]
    pub pair_record_id: String,
    #[serde(rename = "PairRecordData")]
    #[serde(with = "serde_bytes")]
    pub pair_record_data: Vec<u8>,
}

// ── inbound types ────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceEntry {
    #[serde(rename = "DeviceID")]
    pub device_id: u32,
    pub properties: DeviceProperties,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceProperties {
    pub serial_number: Option<String>,
    pub connection_type: Option<String>,
    #[serde(rename = "ProductID")]
    pub product_id: Option<u16>,
    #[serde(rename = "LocationID")]
    pub location_id: Option<u32>,
}

/// Generic envelope — all fields optional so one type handles all responses.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Envelope {
    pub message_type: Option<String>,
    pub number: Option<u32>,
    pub device_list: Option<Vec<DeviceEntry>>,
    #[serde(rename = "BUID")]
    pub buid: Option<String>,
    #[serde(rename = "DeviceID")]
    pub device_id: Option<u32>,
    pub properties: Option<DeviceProperties>,
    #[serde(rename = "PairRecordData")]
    pub pair_record_data: Option<serde_bytes::ByteBuf>,
}

pub fn encode(value: &impl Serialize) -> Result<Vec<u8>, plist::Error> {
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, value)?;
    Ok(buf)
}

pub fn decode(bytes: &[u8]) -> Result<Envelope, plist::Error> {
    plist::from_bytes(bytes)
}
