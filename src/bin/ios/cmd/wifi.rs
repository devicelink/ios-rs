use anyhow::{Context, Result};
use ios_rs::lockdown::services::wireless::WirelessClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

pub fn status(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut client = WirelessClient::connect(session.lockdown())
        .context("connect wireless_lockdown (not available on iOS 18+)")?;
    let enabled = client.get_wifi_enabled().context("get wifi status")?;
    println!("wifi connections: {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

pub fn set(udid: Option<&str>, mode: ConnectionMode, enabled: bool) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut client = WirelessClient::connect(session.lockdown())
        .context("connect wireless_lockdown (not available on iOS 18+)")?;
    client.set_wifi_enabled(enabled).context("set wifi")?;
    println!("wifi connections: {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}
