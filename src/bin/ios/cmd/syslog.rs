use std::io::Write;

use anyhow::{Context, Result};
use ios_rs::lockdown::services::{SyslogClient, SyslogEntry};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::OutputMode;

pub fn run(
    udid:        Option<&str>,
    mode:        ConnectionMode,
    process:     Option<&str>,
    filter:      Option<&str>,
    output:      OutputMode,
    output_file: Option<&str>,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;

    let mut client = if session.is_rsd() {
        let stream = session
            .connect_rsd_shim("com.apple.syslog_relay.shim.remote")
            .context("connect syslog shim")?;
        SyslogClient::from_stream(stream)
    } else {
        SyslogClient::connect(session.lockdown()).context("connect syslog")?
    };

    let json = output.is_json();

    let mut file_out;
    let mut stdout_out;
    let out: &mut dyn Write = if let Some(path) = output_file {
        file_out = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("create {path}"))?
        );
        &mut file_out
    } else {
        stdout_out = std::io::BufWriter::new(std::io::stdout());
        &mut stdout_out
    };

    client.stream(|entry| {
        if !matches_filters(&entry, process, filter) {
            return true;
        }
        let line = if json { to_json(&entry) } else { entry.raw.clone() };
        let ok = writeln!(out, "{line}").and_then(|_| out.flush()).is_ok();
        ok
    })
    .context("syslog stream")?;

    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn matches_filters(entry: &SyslogEntry, process: Option<&str>, filter: Option<&str>) -> bool {
    if let Some(proc_name) = process {
        if !entry.process.to_lowercase().contains(&proc_name.to_lowercase()) {
            return false;
        }
    }
    if let Some(pat) = filter {
        if !entry.raw.to_lowercase().contains(&pat.to_lowercase()) {
            return false;
        }
    }
    true
}

fn to_json(e: &SyslogEntry) -> String {
    format!(
        r#"{{"timestamp":{ts},"process":{proc},"level":{level},"message":{msg}}}"#,
        ts    = json_str(&e.timestamp),
        proc  = json_str(&e.process),
        level = json_str(&e.level),
        msg   = json_str(&e.message),
    )
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
