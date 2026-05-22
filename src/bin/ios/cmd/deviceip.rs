use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

pub fn run(udid: Option<&str>) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;

    // CDTunnel address (device side of the IPv6 tunnel our tooling uses)
    if let Some(t) = session.smoltcp_tunnel_ref() {
        println!("CDTunnel IPv6:  {}", t.params.server_addr);
    }

    // Lockdownd network addresses
    let ld = session.lockdown();
    for (label, key) in [
        ("WiFi MAC:      ", "WiFiAddress"),
        ("Bluetooth MAC: ", "BluetoothAddress"),
        ("Ethernet MAC:  ", "EthernetAddress"),
    ] {
        if let Ok(plist::Value::String(v)) = ld.get_value(None, key) {
            println!("{label}{v}");
        }
    }

    eprintln!("\nFor the device's LAN IP: check your router ARP table or run `arp -a` on the host.");
    Ok(())
}
