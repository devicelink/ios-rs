use anyhow::Result;
use ios_rs::tunnel::detect_version;

use super::resolve_device;

pub fn run(udid: Option<&str>) -> Result<()> {
    let device = resolve_device(udid)?;
    let ver = detect_version(device.device_id)?;

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
