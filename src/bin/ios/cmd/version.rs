use anyhow::Result;
use ios_rs::tunnel::detect_version;

use super::resolve_device;
use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct VersionInfo {
    udid: String,
    ios_version: String,
    rsd_capable: bool,
    active_path: String,
}

pub fn run(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let device = resolve_device(udid)?;
    let ver = detect_version(device.device_id)?;

    if output.is_json() {
        let (rsd_capable, active_path) = if ver.is_legacy() {
            (false, "usbmux → lockdownd".to_string())
        } else if ver.supports_core_device_proxy() {
            (
                true,
                "usbmux → lockdownd → CoreDeviceProxy → CDTunnel → RSD".to_string(),
            )
        } else {
            (
                true,
                "USB-Ethernet (CDC-NCM) → RSD → QUIC tunnel".to_string(),
            )
        };
        let info = VersionInfo {
            udid: device.serial.clone(),
            ios_version: ver.to_string(),
            rsd_capable,
            active_path,
        };
        return print_json(&info);
    }

    println!("Device:      {}", device.serial);
    println!("iOS version: {ver}");
    println!();

    if ver.is_legacy() {
        println!("Path: usbmux → lockdownd (iOS < 17)");
        println!("  All services reachable directly via lockdownd StartService.");
    } else if ver.supports_core_device_proxy() {
        println!("Path: usbmux → lockdownd → CoreDeviceProxy → CDTunnel → RSD (iOS 17.4+)");
        println!("  Tunnel available without USB-Ethernet driver.");
        println!("  Developer services require the RSD service catalogue.");
    } else {
        println!("Path: USB-Ethernet (CDC-NCM) → RSD → QUIC tunnel (iOS 17.0–17.3)");
        println!("  Requires USB-Ethernet driver and RemotePairing handshake.");
        println!("  CoreDeviceProxy shortcut not available on this iOS version.");
    }

    Ok(())
}
