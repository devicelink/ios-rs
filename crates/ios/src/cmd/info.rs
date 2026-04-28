use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use tunnel::ConnectionMode;

use super::open_session;

pub fn run(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let info = session.lockdown().get_all_values()?;

    let udid_str   = if info.unique_device_id.is_empty() { &session.device.serial } else { &info.unique_device_id };
    let serial_str = if info.serial_number.is_empty() { "(requires pairing)" } else { &info.serial_number };

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(["Field", "Value"]);

    let rows: &[(&str, &str)] = &[
        ("UDID",           udid_str),
        ("Name",           &info.device_name),
        ("Product",        &info.product_type),
        ("iOS version",    &info.product_version),
        ("Hardware model", &info.hardware_model),
        ("Serial number",  serial_str),
        ("CPU arch",       &info.cpu_architecture),
        ("Path",           &session.active_path.to_string()),
    ];
    for (k, v) in rows {
        table.add_row([*k, v]);
    }

    let skip = ["ProductVersion", "HumanReadableProductVersionString", "ProductName"];
    let mut extra: Vec<_> = info.extra
        .iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .collect();
    extra.sort_by_key(|(k, _)| k.as_str());
    for (k, v) in extra {
        table.add_row([k.as_str(), &plist_display(v)]);
    }

    println!("{table}");
    Ok(())
}

fn plist_display(v: &plist::Value) -> String {
    match v {
        plist::Value::String(s)  => s.clone(),
        plist::Value::Boolean(b) => b.to_string(),
        plist::Value::Integer(i) => i.to_string(),
        plist::Value::Real(f)    => format!("{f}"),
        plist::Value::Array(arr) => {
            let items: Vec<_> = arr.iter().map(plist_display).collect();
            items.join(", ")
        }
        plist::Value::Dictionary(d) => format!("{{{}  keys}}", d.len()),
        plist::Value::Data(b) => format!("<{} bytes>", b.len()),
        plist::Value::Date(d) => format!("{d:?}"),
        _ => format!("{v:?}"),
    }
}
