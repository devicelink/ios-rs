use std::path::Path;

use anyhow::{bail, Context, Result};
use ios_rs::lockdown::services::{AfcClient, FileType};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

// ── JSON schemas ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct AfcEntry {
    name:  String,
    #[serde(rename = "type")]
    kind:  String,
    size:  Option<u64>,
    mtime: Option<u64>,
}

#[derive(serde::Serialize)]
struct StatResult {
    #[serde(rename = "type")]
    kind:  String,
    size:  u64,
    mtime: u64,
}

#[derive(serde::Serialize)]
struct AfcInfo {
    model:       String,
    total_bytes: u64,
    free_bytes:  u64,
}

#[derive(serde::Serialize)]
struct PullResult {
    ok:   bool,
    path: String,
}

// ── connect helper ────────────────────────────────────────────────────────────

/// Open an AFC connection.  If `app` is `Some`, connects to that app's sandbox
/// container via house-arrest; otherwise connects to the media partition.
///
/// On iOS 17.4+ the house-arrest service is only accessible via the RSD shim
/// (`com.apple.mobile.house_arrest.shim.remote`); this is selected
/// automatically when the session is on the RSD path.
fn connect(
    udid: Option<&str>,
    mode: ConnectionMode,
    app:  Option<&str>,
) -> Result<(ios_rs::tunnel::DeviceSession, AfcClient)> {
    let mut session = open_session(udid, mode)?;
    let afc = match app {
        Some(bundle_id) => {
            if session.is_rsd() {
                let stream = session
                    .connect_rsd_shim("com.apple.mobile.house_arrest.shim.remote")
                    .with_context(|| format!("connect house_arrest shim for {bundle_id}"))?;
                AfcClient::connect_app_shim(stream, bundle_id)
                    .with_context(|| format!("house_arrest handshake for {bundle_id}"))?
            } else {
                AfcClient::connect_app(session.lockdown(), bundle_id)
                    .with_context(|| format!("connect app container {bundle_id}"))?
            }
        }
        None => AfcClient::connect(session.lockdown()).context("connect AFC")?,
    };
    Ok((session, afc))
}

// ── entry points ──────────────────────────────────────────────────────────────

pub fn ls(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    path:   &str,
    long:   bool,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;

    let mut entries = afc.list_dir(path).with_context(|| format!("ls {path}"))?;
    entries.sort();

    if output.is_json() {
        let mut json_entries = Vec::new();
        for name in &entries {
            let full = format!("{}/{}", path.trim_end_matches('/'), name);
            let (kind, size, mtime) = match afc.get_file_info(&full) {
                Ok(info) => {
                    let kind_str = match info.file_type {
                        FileType::Regular   => "file",
                        FileType::Directory => "directory",
                        FileType::Symlink   => "symlink",
                        FileType::Other     => "other",
                    }.to_string();
                    let sz = if info.file_type == FileType::Regular { Some(info.size) } else { None };
                    let mt = Some(info.modified_nanos / 1_000_000_000);
                    (kind_str, sz, mt)
                }
                Err(_) => ("unknown".to_string(), None, None),
            };
            json_entries.push(AfcEntry { name: name.clone(), kind, size, mtime });
        }
        return print_json(&json_entries);
    }

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
                    let extra = info.link_target
                        .as_deref()
                        .map(|t| format!(" -> {t}"))
                        .unwrap_or_default();
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

pub fn stat(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    path:   &str,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;

    let info = afc.get_file_info(path).with_context(|| format!("stat {path}"))?;
    let type_str = match info.file_type {
        FileType::Regular   => "file",
        FileType::Directory => "directory",
        FileType::Symlink   => "symlink",
        FileType::Other     => "other",
    };

    if output.is_json() {
        return print_json(&StatResult {
            kind:  type_str.to_string(),
            size:  info.size,
            mtime: info.modified_nanos / 1_000_000_000,
        });
    }

    println!("type:     {type_str}");
    println!("size:     {} ({} bytes)", human_size(info.size), info.size);
    println!("modified: {} (unix ns)", info.modified_nanos);
    if let Some(target) = info.link_target {
        println!("target:   {target}");
    }
    Ok(())
}

pub fn info(udid: Option<&str>, mode: ConnectionMode, app: Option<&str>, output: OutputMode) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;

    let dev = afc.device_info().context("device_info")?;

    if output.is_json() {
        return print_json(&AfcInfo {
            model:       dev.model.clone(),
            total_bytes: dev.total_bytes,
            free_bytes:  dev.free_bytes,
        });
    }

    println!("model:      {}", dev.model);
    println!("total:      {}", human_size(dev.total_bytes));
    println!("free:       {}", human_size(dev.free_bytes));
    println!("block size: {} bytes", dev.block_size);
    Ok(())
}

pub fn pull(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    remote: &str,
    local:  &Path,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;

    let dest = if local.is_dir() {
        local.join(leaf(remote))
    } else {
        local.to_path_buf()
    };

    afc.pull_file(remote, &dest, |done, total| {
        if total > 0 && !output.is_json() {
            eprint!("\r  {}/{}", human_size(done), human_size(total));
        }
    }).with_context(|| format!("pull {remote}"))?;

    if output.is_json() {
        return print_json(&PullResult { ok: true, path: dest.display().to_string() });
    }

    eprintln!();
    println!("→ {}", dest.display());
    Ok(())
}

pub fn push(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    local:  &Path,
    remote: &str,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    if !local.exists() {
        bail!("{} does not exist", local.display());
    }
    let (_session, mut afc) = connect(udid, mode, app)?;

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

    if output.is_json() {
        return print_json(&ActionResult::with_msg(format!("→ {dest}")));
    }

    println!("→ {dest}");
    Ok(())
}

pub fn rm(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    path:   &str,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;
    afc.remove_path(path).with_context(|| format!("rm {path}"))?;
    if output.is_json() {
        print_json(&ActionResult::ok())?;
    }
    Ok(())
}

pub fn mkdir(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    path:   &str,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;
    afc.mkdir(path).with_context(|| format!("mkdir {path}"))?;
    if output.is_json() {
        print_json(&ActionResult::ok())?;
    }
    Ok(())
}

pub fn mv(
    udid:   Option<&str>,
    mode:   ConnectionMode,
    from:   &str,
    to:     &str,
    app:    Option<&str>,
    output: OutputMode,
) -> Result<()> {
    let (_session, mut afc) = connect(udid, mode, app)?;
    afc.rename(from, to).with_context(|| format!("mv {from} -> {to}"))?;
    if output.is_json() {
        print_json(&ActionResult::ok())?;
    }
    Ok(())
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
