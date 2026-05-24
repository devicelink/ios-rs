use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ios_rs::tunnel::ConnectionMode;
use plist::Value;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

#[derive(serde::Serialize)]
struct DateInfo {
    timezone: Option<String>,
}

pub fn run(
    udid: Option<&str>,
    timezone: Option<&str>,
    sync_time: bool,
    mode: ConnectionMode,
    output: OutputMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let ld = session.lockdown();

    if timezone.is_none() && !sync_time {
        // Display current timezone
        let tz = ld.get_value(None, "TimeZone").ok().and_then(|v| {
            if let Value::String(s) = v {
                Some(s)
            } else {
                None
            }
        });

        if output.is_json() {
            return print_json(&DateInfo { timezone: tz });
        }
        println!("TimeZone: {}", tz.as_deref().unwrap_or("-"));
        return Ok(());
    }

    if sync_time {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ld.set_value(None, "TimeIntervalSince1970", Value::Integer(secs.into()))?;
        if !output.is_json() {
            println!("Device time synced to host time ({secs}).");
        }
    }

    if let Some(tz) = timezone {
        ld.set_value(None, "TimeZone", Value::String(tz.into()))?;
        if !output.is_json() {
            println!("TimeZone set to '{tz}'.");
        }
    }

    if output.is_json() {
        print_json(&ActionResult::ok())?;
    }

    Ok(())
}
