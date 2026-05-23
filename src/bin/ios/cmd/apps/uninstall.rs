use anyhow::Result;
use ios_rs::lockdown::services::InstallationProxy;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

pub fn run(udid: Option<&str>, bundle_id: &str, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut proxy   = InstallationProxy::connect(session.lockdown())?;

    eprintln!("Uninstalling {bundle_id}…");
    proxy.uninstall(bundle_id)?;

    if output.is_json() {
        print_json(&ActionResult::with_msg("uninstalled"))?;
    } else {
        println!("Done.");
    }
    Ok(())
}
