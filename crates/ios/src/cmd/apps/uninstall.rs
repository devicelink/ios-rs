use anyhow::Result;
use lockdown::services::InstallationProxy;
use tunnel::ConnectionMode;

use crate::cmd::open_session;

pub fn run(udid: Option<&str>, bundle_id: &str, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut proxy   = InstallationProxy::connect(session.lockdown())?;

    eprintln!("Uninstalling {bundle_id}…");
    proxy.uninstall(bundle_id)?;
    println!("Done.");
    Ok(())
}
