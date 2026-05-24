use anyhow::{Context, Result};
use ios_rs::lockdown::services::diagnostics::DiagnosticsClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{plist_to_json, print_json, ActionResult, OutputMode};

fn client(
    udid: Option<&str>,
    mode: ConnectionMode,
) -> Result<(ios_rs::tunnel::DeviceSession, DiagnosticsClient)> {
    let mut session = open_session(udid, mode)?;
    let diag = if session.is_rsd() {
        let stream = session
            .connect_rsd_shim("com.apple.mobile.diagnostics_relay.shim.remote")
            .context("connect diagnostics shim")?;
        DiagnosticsClient::from_stream(stream)
    } else {
        DiagnosticsClient::connect(session.lockdown()).context("connect diagnostics")?
    };
    Ok((session, diag))
}

pub fn reboot(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    diag.restart().context("reboot")?;
    if output.is_json() {
        print_json(&ActionResult::with_msg("rebooting"))?;
    } else {
        eprintln!("rebooting…");
    }
    Ok(())
}

pub fn shutdown(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    diag.shutdown().context("shutdown")?;
    if output.is_json() {
        print_json(&ActionResult::with_msg("shutting down"))?;
    } else {
        eprintln!("shutting down…");
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct BatteryInfo {
    capacity_pct: u64,
    voltage_mv: u64,
    cycle_count: u64,
    design_capacity: u64,
    full_capacity: u64,
    is_charging: bool,
    external_connected: bool,
    fully_charged: bool,
}

pub fn battery(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    let info = diag.battery().context("battery info")?;

    if output.is_json() {
        let bi = BatteryInfo {
            capacity_pct: info.capacity_pct,
            voltage_mv: info.voltage_mv,
            cycle_count: info.cycle_count,
            design_capacity: info.design_capacity,
            full_capacity: info.full_capacity,
            is_charging: info.is_charging,
            external_connected: info.external_connected,
            fully_charged: info.fully_charged,
        };
        return print_json(&bi);
    }

    if info.capacity_pct > 0 {
        println!("capacity:    {}%", info.capacity_pct);
    }
    if info.voltage_mv > 0 {
        println!("voltage:     {} mV", info.voltage_mv);
    }
    if info.cycle_count > 0 {
        println!("cycle count: {}", info.cycle_count);
    }
    if info.design_capacity > 0 {
        println!("design cap:  {} mAh", info.design_capacity);
    }
    // On iOS 18+ FullChargeCapacity is reported as a health percentage (100 = perfect)
    if info.full_capacity > 0 {
        println!("health:      {}%", info.full_capacity);
    }
    if info.is_charging {
        println!("charging:    true");
    }
    if info.external_connected {
        println!("plugged in:  true");
    }
    if info.fully_charged {
        println!("full charge: true");
    }
    Ok(())
}

pub fn all(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    let dict = diag.all().context("diagnostics all")?;

    if output.is_json() {
        let jval = plist_to_json(&plist::Value::Dictionary(dict));
        return print_json(&jval);
    }

    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, &plist::Value::Dictionary(dict))?;
    print!("{}", String::from_utf8_lossy(&buf));
    Ok(())
}
