use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;

use super::open_session;
use crate::cmd::output::{print_json, OutputMode};

pub fn run(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut services = session.lockdown().list_services()?;
    services.sort();

    if output.is_json() {
        return print_json(&services);
    }

    for svc in services {
        println!("{svc}");
    }
    Ok(())
}
