use super::Error;

/// Parsed lockdownd pair record.
///
/// Obtained from usbmuxd via `ReadPairRecord`.  Contains the mutual TLS
/// credentials established when the user tapped "Trust" on the device.
#[derive(Debug, Clone)]
pub struct PairRecord {
    pub host_id:            String,
    pub system_buid:        String,
    /// PEM-encoded X.509 certificate we present as client cert in TLS
    pub host_certificate:   Vec<u8>,
    /// PEM-encoded RSA private key matching host_certificate
    pub host_private_key:   Vec<u8>,
    /// PEM-encoded device certificate (server side of TLS)
    pub device_certificate: Vec<u8>,
    /// PEM-encoded root CA that signed both host and device certs
    pub root_certificate:   Vec<u8>,
}

impl PairRecord {
    /// Parse from the raw plist bytes returned by usbmuxd `ReadPairRecord`.
    pub fn from_plist_bytes(data: &[u8]) -> Result<Self, Error> {
        let val: plist::Value = plist::from_bytes(data)
            .map_err(|e| Error::PairRecord(format!("plist parse: {e}")))?;
        let dict = val.as_dictionary()
            .ok_or_else(|| Error::PairRecord("not a dictionary".into()))?;

        let host_id = string(dict, "HostID")?;
        let system_buid = string(dict, "SystemBUID")?;
        let host_certificate   = data_field(dict, "HostCertificate")?;
        let host_private_key   = data_field(dict, "HostPrivateKey")?;
        let device_certificate = data_field(dict, "DeviceCertificate")?;
        let root_certificate   = data_field(dict, "RootCertificate")?;

        Ok(PairRecord {
            host_id,
            system_buid,
            host_certificate,
            host_private_key,
            device_certificate,
            root_certificate,
        })
    }

    /// Fetch the pair record for `udid` from the running usbmuxd and parse it.
    pub fn read_from_usbmuxd(udid: &str) -> Result<Self, Error> {
        let mut conn = crate::usbmux::Connection::open()?;
        let raw = conn.read_pair_record(udid)?;
        Self::from_plist_bytes(&raw)
    }
}

fn string(dict: &plist::Dictionary, key: &str) -> Result<String, Error> {
    dict.get(key)
        .and_then(|v| v.as_string())
        .map(|s| s.to_owned())
        .ok_or_else(|| Error::PairRecord(format!("missing string key {key}")))
}

fn data_field(dict: &plist::Dictionary, key: &str) -> Result<Vec<u8>, Error> {
    dict.get(key)
        .and_then(|v| match v {
            plist::Value::Data(b) => Some(b.clone()),
            // Some implementations store certs as strings (PEM text)
            plist::Value::String(s) => Some(s.as_bytes().to_vec()),
            _ => None,
        })
        .ok_or_else(|| Error::PairRecord(format!("missing data key {key}")))
}
