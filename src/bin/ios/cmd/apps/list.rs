use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use ios_rs::lockdown::services::{AppType, InstallationProxy};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, OutputMode};

const RSD_SERVICE: &str = "com.apple.mobile.installation_proxy.shim.remote";

#[derive(serde::Serialize)]
struct AppEntry {
    bundle_id: String,
    name:      String,
    version:   String,
}

pub fn run(udid: Option<&str>, system: bool, all: bool, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;

    let mut proxy = if session.is_rsd() {
        let stream = session.connect_rsd_shim(RSD_SERVICE)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        InstallationProxy::from_stream(stream)
    } else {
        InstallationProxy::connect(session.lockdown())
            .map_err(|e| anyhow::anyhow!("{e}"))?
    };

    let app_type = if all { AppType::Any } else if system { AppType::System } else { AppType::User };
    let mut apps = proxy.list_apps(app_type)?;
    apps.sort_by(|a, b| a.name.cmp(&b.name));

    if output.is_json() {
        let entries: Vec<AppEntry> = apps.iter().map(|app| AppEntry {
            bundle_id: app.bundle_id.clone(),
            name:      app.name.clone(),
            version:   app.short_version.clone(),
        }).collect();
        return print_json(&entries);
    }

    if apps.is_empty() {
        println!("No apps found.");
        return Ok(());
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(["Bundle ID", "Name", "Version"]);
    for app in &apps {
        table.add_row([&app.bundle_id, &app.name, &app.short_version]);
    }
    println!("{table}");
    println!("({} apps)", apps.len());
    Ok(())
}
