use std::io::Write;

use anyhow::{Context, Result};
use ios_rs::lockdown::services::screenshot::ScreenshotClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

pub fn run(udid: Option<&str>, mode: ConnectionMode, output: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    // com.apple.mobile.screenshotr is a lockdownd-only service removed on iOS 17+.
    // Modern iOS uses com.apple.corecaptured.remoteservice (remote-XPC, not yet implemented).
    let mut client  = ScreenshotClient::connect(session.lockdown())
        .context("connect screenshotr — note: screenshot requires iOS < 17 (see TODO.md)")?;

    let png = client.take().context("take screenshot")?;

    if output == "-" {
        std::io::stdout().write_all(&png).context("write PNG to stdout")?;
    } else {
        std::fs::write(output, &png)
            .with_context(|| format!("write {output}"))?;
        eprintln!("saved {} bytes → {output}", png.len());
    }
    Ok(())
}
