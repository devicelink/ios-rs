use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use ios_rs::lockdown::LockdownSession;
use ios_rs::usbmux::Connection;

use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct DeviceEntry {
    udid:        String,
    name:        String,
    connection:  String,
    ios_version: String,
}

pub fn run(output: OutputMode) -> Result<()> {
    let mut conn = Connection::open()?;
    let devices = conn.list_devices()?;

    if devices.is_empty() {
        if output.is_json() {
            let empty: Vec<DeviceEntry> = vec![];
            return print_json(&empty);
        }
        println!("No devices connected.");
        return Ok(());
    }

    if output.is_json() {
        let entries: Vec<DeviceEntry> = devices.iter().map(|d| {
            let (name, version) = fetch_display_info(d.device_id);
            DeviceEntry {
                udid:        d.serial.clone(),
                name,
                connection:  d.connection_type.to_string(),
                ios_version: version,
            }
        }).collect();
        return print_json(&entries);
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(["Name", "UDID", "Connection", "iOS"]);

    for d in &devices {
        // Best-effort: fetch name + version from lockdownd (no pairing needed)
        let (name, version) = fetch_display_info(d.device_id);
        table.add_row([
            name.as_str(),
            d.serial.as_str(),
            &d.connection_type.to_string(),
            version.as_str(),
        ]);
    }

    println!("{table}");
    Ok(())
}

fn fetch_display_info(device_id: u32) -> (String, String) {
    let name = || "—".to_string();
    let ver  = || "—".to_string();

    let Ok(mut s) = LockdownSession::connect(device_id) else { return (name(), ver()); };

    let n = s.get_value(None, "DeviceName")
        .ok()
        .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None })
        .unwrap_or_else(name);

    let v = s.get_value(None, "ProductVersion")
        .ok()
        .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None })
        .unwrap_or_else(ver);

    (n, v)
}
