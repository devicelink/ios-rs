use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;

use super::open_session;
use crate::cmd::output::{print_json, OutputMode};

#[derive(serde::Serialize)]
struct RsdService {
    name: String,
    port: u16,
    uses_remote_xpc: bool,
}

pub fn run(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let rsd = session.connect_rsd()?;

    let info = rsd.peer_info();

    if output.is_json() {
        let mut svcs: Vec<RsdService> = rsd
            .services()
            .iter()
            .map(|(name, entry)| RsdService {
                name: name.clone(),
                port: entry.port,
                uses_remote_xpc: entry.uses_remote_xpc,
            })
            .collect();
        svcs.sort_by(|a, b| a.name.cmp(&b.name));
        return print_json(&svcs);
    }

    println!(
        "RSD peer: {} {} ({})",
        info.product_type, info.os_version, info.udid
    );
    println!();

    let mut svcs: Vec<_> = rsd.services().iter().collect();
    svcs.sort_by_key(|(name, _)| name.as_str());
    println!("{:<60} {:>6}  remote-xpc", "Service", "Port");
    println!("{}", "─".repeat(72));
    for (name, entry) in svcs {
        println!(
            "{:<60} {:>6}  {}",
            name,
            entry.port,
            if entry.uses_remote_xpc { "yes" } else { "no" }
        );
    }
    Ok(())
}
