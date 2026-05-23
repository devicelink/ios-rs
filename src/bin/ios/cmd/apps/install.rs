use std::path::Path;

use anyhow::{bail, Result};
use ios_rs::lockdown::services::{AfcClient, InstallationProxy};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

pub fn run(udid: Option<&str>, ipa_path: &str, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let ipa = Path::new(ipa_path);
    if !ipa.exists() { bail!("IPA not found: {ipa_path}"); }
    if ipa.extension().and_then(|e| e.to_str()) != Some("ipa") {
        bail!("expected a .ipa file, got: {ipa_path}");
    }

    let data     = std::fs::read(ipa)?;
    let filename = ipa.file_name().unwrap().to_string_lossy();
    let staged   = format!("/PublicStaging/{filename}");

    let mut session = open_session(udid, mode)?;

    eprintln!("Uploading {} ({:.1} MB) via AFC…", filename, data.len() as f64 / 1_048_576.0);
    let mut afc = AfcClient::connect(session.lockdown())?;
    afc.put_file(&staged, &data)?;
    eprintln!("Upload complete → {staged}");

    eprintln!("Installing…");
    let mut proxy = InstallationProxy::connect(session.lockdown())?;
    proxy.install_staged(&staged)?;

    if output.is_json() {
        print_json(&ActionResult::with_msg("installed"))?;
    } else {
        println!("Done.");
    }
    Ok(())
}
