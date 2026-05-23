use anyhow::{Context, Result};
use ios_rs::lockdown::services::amfi::AmfiClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct DevmodeStatus {
    enabled: bool,
}

#[derive(serde::Serialize)]
struct DevmodeEnabled {
    ok:              bool,
    reboot_required: bool,
}

pub fn status(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut amfi = AmfiClient::connect(session.lockdown()).context("connect amfi")?;
    let enabled = amfi.developer_mode_status().context("developer mode status")?;

    if output.is_json() {
        return print_json(&DevmodeStatus { enabled });
    }

    println!("developer mode: {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

pub fn enable(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut amfi = AmfiClient::connect(session.lockdown()).context("connect amfi")?;
    let reboot = amfi.enable_developer_mode().context("enable developer mode")?;

    if output.is_json() {
        return print_json(&DevmodeEnabled { ok: true, reboot_required: reboot });
    }

    if reboot {
        println!("developer mode enabled — reboot required");
    } else {
        println!("developer mode enabled");
    }
    Ok(())
}
