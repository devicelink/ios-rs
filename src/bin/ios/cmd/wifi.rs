use anyhow::{Context, Result};
use ios_rs::lockdown::services::wireless::WirelessClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

#[derive(serde::Serialize)]
struct WifiStatus {
    enabled: bool,
}

pub fn status(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut client = WirelessClient::connect(session.lockdown())
        .context("connect wireless_lockdown (not available on iOS 18+)")?;
    let enabled = client.get_wifi_enabled().context("get wifi status")?;

    if output.is_json() {
        return print_json(&WifiStatus { enabled });
    }

    println!(
        "wifi connections: {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

pub fn set(
    udid: Option<&str>,
    mode: ConnectionMode,
    enabled: bool,
    output: OutputMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut client = WirelessClient::connect(session.lockdown())
        .context("connect wireless_lockdown (not available on iOS 18+)")?;
    client.set_wifi_enabled(enabled).context("set wifi")?;

    if output.is_json() {
        return print_json(&ActionResult::with_msg(if enabled {
            "wifi enabled"
        } else {
            "wifi disabled"
        }));
    }

    println!(
        "wifi connections: {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}
