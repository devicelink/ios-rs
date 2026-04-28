use anyhow::Result;
use tunnel::ConnectionMode;

use super::open_session;

pub fn run(udid: Option<&str>) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let rsd = session.connect_rsd()?;

    let info = rsd.peer_info();
    println!("RSD peer: {} {} ({})", info.product_type, info.os_version, info.udid);
    println!();

    let mut svcs: Vec<_> = rsd.services().iter().collect();
    svcs.sort_by_key(|(name, _)| name.as_str());
    println!("{:<60} {:>6}  remote-xpc", "Service", "Port");
    println!("{}", "─".repeat(72));
    for (name, entry) in svcs {
        println!("{:<60} {:>6}  {}",
            name, entry.port,
            if entry.uses_remote_xpc { "yes" } else { "no" });
    }
    Ok(())
}
