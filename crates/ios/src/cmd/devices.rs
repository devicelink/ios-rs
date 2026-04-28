use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use lockdown::LockdownSession;
use usbmux::Connection;

pub fn run() -> Result<()> {
    let mut conn = Connection::open()?;
    let devices = conn.list_devices()?;

    if devices.is_empty() {
        println!("No devices connected.");
        return Ok(());
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
