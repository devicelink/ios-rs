use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct DeviceIpInfo {
    cdtunnel_ipv6:  Option<String>,
    wifi_mac:       Option<String>,
    bluetooth_mac:  Option<String>,
    ethernet_mac:   Option<String>,
}

pub fn run(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;

    // CDTunnel address (device side of the IPv6 tunnel our tooling uses)
    let cdtunnel_ipv6 = session.smoltcp_tunnel_ref()
        .map(|t| t.params.server_addr.to_string());

    // Lockdownd network addresses
    let ld = session.lockdown();
    let wifi_mac = ld.get_value(None, "WiFiAddress").ok()
        .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None });
    let bluetooth_mac = ld.get_value(None, "BluetoothAddress").ok()
        .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None });
    let ethernet_mac = ld.get_value(None, "EthernetAddress").ok()
        .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None });

    if output.is_json() {
        let info = DeviceIpInfo {
            cdtunnel_ipv6,
            wifi_mac,
            bluetooth_mac,
            ethernet_mac,
        };
        return print_json(&info);
    }

    if let Some(ref addr) = cdtunnel_ipv6 {
        println!("CDTunnel IPv6:  {addr}");
    }
    for (label, val) in [
        ("WiFi MAC:      ", &wifi_mac),
        ("Bluetooth MAC: ", &bluetooth_mac),
        ("Ethernet MAC:  ", &ethernet_mac),
    ] {
        if let Some(v) = val {
            println!("{label}{v}");
        }
    }

    eprintln!("\nFor the device's LAN IP: check your router ARP table or run `arp -a` on the host.");
    Ok(())
}
