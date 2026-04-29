use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use plist::Value;
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;

pub fn run(
    udid:     Option<&str>,
    timezone: Option<&str>,
    sync_time: bool,
    mode:     ConnectionMode,
) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let ld = session.lockdown();

    if timezone.is_none() && !sync_time {
        // Display current timezone
        let tz = ld.get_value(None, "TimeZone")?;
        println!("TimeZone: {}", tz.as_string().unwrap_or("-"));
        return Ok(());
    }

    if sync_time {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ld.set_value(None, "TimeIntervalSince1970", Value::Integer(secs.into()))?;
        println!("Device time synced to host time ({secs}).");
    }

    if let Some(tz) = timezone {
        ld.set_value(None, "TimeZone", Value::String(tz.into()))?;
        println!("TimeZone set to '{tz}'.");
    }

    Ok(())
}
