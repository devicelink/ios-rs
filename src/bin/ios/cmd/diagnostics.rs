use anyhow::{Context, Result};
use ios_rs::lockdown::services::diagnostics::DiagnosticsClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

fn client(udid: Option<&str>, mode: ConnectionMode) -> Result<(ios_rs::tunnel::DeviceSession, DiagnosticsClient)> {
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

pub fn reboot(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    diag.restart().context("reboot")?;
    eprintln!("rebooting…");
    Ok(())
}

pub fn shutdown(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    diag.shutdown().context("shutdown")?;
    eprintln!("shutting down…");
    Ok(())
}

pub fn battery(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    let info = diag.battery().context("battery info")?;
    if info.capacity_pct > 0  { println!("capacity:    {}%",    info.capacity_pct); }
    if info.voltage_mv > 0    { println!("voltage:     {} mV",  info.voltage_mv); }
    if info.cycle_count > 0   { println!("cycle count: {}",     info.cycle_count); }
    if info.design_capacity > 0 { println!("design cap:  {} mAh", info.design_capacity); }
    // On iOS 18+ FullChargeCapacity is reported as a health percentage (100 = perfect)
    if info.full_capacity > 0 { println!("health:      {}%",    info.full_capacity); }
    if info.is_charging        { println!("charging:    true"); }
    if info.external_connected { println!("plugged in:  true"); }
    if info.fully_charged      { println!("full charge: true"); }
    Ok(())
}

pub fn all(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let (_session, mut diag) = client(udid, mode)?;
    let dict = diag.all().context("diagnostics all")?;
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, &plist::Value::Dictionary(dict))?;
    print!("{}", String::from_utf8_lossy(&buf));
    Ok(())
}
