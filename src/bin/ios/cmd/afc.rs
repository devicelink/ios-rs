use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ios_rs::lockdown::services::{AfcClient, FileType};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

// ── entry points ──────────────────────────────────────────────────────────────

pub fn ls(udid: Option<&str>, mode: ConnectionMode, path: &str, long: bool) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;

    let mut entries = afc.list_dir(path).with_context(|| format!("ls {path}"))?;
    entries.sort();

    for name in &entries {
        let full = format!("{}/{}", path.trim_end_matches('/'), name);
        if long {
            match afc.get_file_info(&full) {
                Ok(info) => {
                    let secs = info.modified_nanos / 1_000_000_000;
                    let size_col = if info.file_type == FileType::Regular {
                        human_size(info.size)
                    } else {
                        "         ".into()
                    };
                    let extra = if let Some(target) = &info.link_target {
                        format!(" -> {target}")
                    } else {
                        String::new()
                    };
                    println!("{} {}  {}  {name}{extra}",
                        info.file_type.indicator(), size_col, secs);
                }
                Err(_) => println!("?                    {name}"),
            }
        } else {
            println!("{name}");
        }
    }
    Ok(())
}

pub fn stat(udid: Option<&str>, mode: ConnectionMode, path: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;

    let info = afc.get_file_info(path).with_context(|| format!("stat {path}"))?;
    let type_str = match info.file_type {
        FileType::Regular   => "file",
        FileType::Directory => "directory",
        FileType::Symlink   => "symlink",
        FileType::Other     => "other",
    };
    println!("type:     {type_str}");
    println!("size:     {} ({} bytes)", human_size(info.size), info.size);
    println!("modified: {} (unix ns)", info.modified_nanos);
    if let Some(target) = info.link_target {
        println!("target:   {target}");
    }
    Ok(())
}

pub fn info(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;

    let dev = afc.device_info().context("device_info")?;
    println!("model:      {}", dev.model);
    println!("total:      {}", human_size(dev.total_bytes));
    println!("free:       {}", human_size(dev.free_bytes));
    println!("block size: {} bytes", dev.block_size);
    Ok(())
}

pub fn pull(
    udid: Option<&str>,
    mode: ConnectionMode,
    remote: &str,
    local: &Path,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;

    // If local is an existing directory, download into it using the remote leaf name.
    let dest = if local.is_dir() {
        local.join(leaf(remote))
    } else {
        local.to_path_buf()
    };

    afc.pull_file(remote, &dest, |done, total| {
        if total > 0 {
            eprint!("\r  {}/{}", human_size(done), human_size(total));
        }
    }).with_context(|| format!("pull {remote}"))?;
    eprintln!();
    println!("→ {}", dest.display());
    Ok(())
}

pub fn push(
    udid: Option<&str>,
    mode: ConnectionMode,
    local: &Path,
    remote: &str,
) -> Result<()> {
    if !local.exists() {
        bail!("{} does not exist", local.display());
    }
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;

    // If remote ends with '/', or is an existing remote directory, push into it.
    let dest = if remote.ends_with('/') {
        let name = local.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{}/{}", remote.trim_end_matches('/'), name)
    } else {
        remote.to_owned()
    };

    afc.push_file(local, &dest)
        .with_context(|| format!("push {}", local.display()))?;
    println!("→ {dest}");
    Ok(())
}

pub fn rm(udid: Option<&str>, mode: ConnectionMode, path: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;
    afc.remove_path(path).with_context(|| format!("rm {path}"))
}

pub fn mkdir(udid: Option<&str>, mode: ConnectionMode, path: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;
    afc.mkdir(path).with_context(|| format!("mkdir {path}"))
}

pub fn mv(udid: Option<&str>, mode: ConnectionMode, from: &str, to: &str) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut afc = AfcClient::connect(session.lockdown()).context("connect AFC")?;
    afc.rename(from, to).with_context(|| format!("mv {from} -> {to}"))
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn leaf(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

// Keep PathBuf in scope for the function signatures.
#[allow(dead_code)]
fn _ensure_pathbuf(_: PathBuf) {}
