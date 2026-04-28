use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;

use super::open_session;

pub fn run(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut services = session.lockdown().list_services()?;
    services.sort();
    for svc in services {
        println!("{svc}");
    }
    Ok(())
}
