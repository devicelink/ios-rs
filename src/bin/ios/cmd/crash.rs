//! Crash report access via `com.apple.crashreportcopymobile.shim.remote`.
//!
//! The service speaks the AFC protocol, so we connect through the RSD shim and
//! hand the stream to `AfcClient::from_stream`.
use std::path::Path;

use anyhow::{Context, Result};
use ios_rs::lockdown::services::AfcClient;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

const SHIM: &str = "com.apple.crashreportcopymobile.shim.remote";

fn afc(udid: Option<&str>, mode: ConnectionMode) -> Result<(ios_rs::tunnel::DeviceSession, AfcClient)> {
    let mut session = open_session(udid, mode)?;
    let stream = session.connect_rsd_shim(SHIM).context("connect crash report shim")?;
    Ok((session, AfcClient::from_stream(stream)))
}

pub fn ls(udid: Option<&str>, mode: ConnectionMode, long: bool) -> Result<()> {
    let (_session, mut client) = afc(udid, mode)?;
    let mut entries = client.list_dir("/").context("list crash reports")?;
    entries.sort();
    for name in &entries {
        if long {
            let path = format!("/{name}");
            match client.get_file_info(&path) {
                Ok(info) => {
                    let size = if info.file_type == ios_rs::lockdown::services::FileType::Regular {
                        format!("{:>10}", info.size)
                    } else {
                        format!("{:>10}", "")
                    };
                    println!("{} {}  {name}", info.file_type.indicator(), size);
                }
                Err(_) => println!("?            {name}"),
            }
        } else {
            println!("{name}");
        }
    }
    Ok(())
}

pub fn pull(
    udid:  Option<&str>,
    mode:  ConnectionMode,
    name:  &str,
    local: Option<&str>,
) -> Result<()> {
    let (_session, mut client) = afc(udid, mode)?;
    let remote = format!("/{name}");
    let dest_str = local.unwrap_or(name);
    let dest = Path::new(dest_str);
    client.pull_file(&remote, dest, |_, _| {}).with_context(|| format!("pull {name}"))?;
    eprintln!("→ {dest_str}");
    Ok(())
}

pub fn rm(udid: Option<&str>, mode: ConnectionMode, name: &str) -> Result<()> {
    let (_session, mut client) = afc(udid, mode)?;
    let remote = format!("/{name}");
    client.remove_path(&remote).with_context(|| format!("rm {name}"))
}
