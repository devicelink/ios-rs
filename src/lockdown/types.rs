use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ServiceInfo {
    pub port: u16,
    #[serde(default)]
    pub enable_service_ssl: bool,
}

/// Flattened device info returned by GetValue(nil, nil)
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_name: String,
    pub product_type: String,
    pub product_version: String,
    pub serial_number: String,
    pub hardware_model: String,
    pub unique_device_id: String,
    pub cpu_architecture: String,
    pub extra: HashMap<String, plist::Value>,
}

impl DeviceInfo {
    pub(crate) fn from_dict(mut d: plist::Dictionary) -> Self {
        fn take_str(d: &mut plist::Dictionary, k: &str) -> String {
            d.remove(k)
                .and_then(|v| {
                    if let plist::Value::String(s) = v {
                        Some(s)
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        }
        DeviceInfo {
            device_name: take_str(&mut d, "DeviceName"),
            product_type: take_str(&mut d, "ProductType"),
            product_version: take_str(&mut d, "ProductVersion"),
            serial_number: take_str(&mut d, "SerialNumber"),
            hardware_model: take_str(&mut d, "HardwareModel"),
            unique_device_id: take_str(&mut d, "UniqueDeviceID"),
            cpu_architecture: take_str(&mut d, "CPUArchitecture"),
            extra: d.into_iter().collect(),
        }
    }
}
