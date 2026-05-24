pub mod activate;
pub mod afc;
pub mod apps;
pub mod crash;
pub mod deviceip;
pub mod devices;
pub mod devmode;
pub mod diagnostics;
pub mod erase;
pub mod info;
pub mod lang;
pub mod location;
pub mod mobilegestalt;
pub mod mounter;
pub mod notification;
pub mod orientation;
pub mod oslog;
pub mod output;
pub mod pair;
pub mod perf;
pub mod ps;
pub mod relay;
pub mod rsd;
pub mod runtest;
pub mod screenshot;
pub mod services;
pub mod setup;
pub mod syslog;
pub mod timezone;
pub mod tunnel;
pub mod version;
pub mod watch;
pub mod wifi;

use anyhow::{bail, Result};
use ios_rs::tunnel::{ConnectionMode, DeviceSession};
use ios_rs::usbmux::{Connection, Device};

/// Resolve a device by UDID or pick the first connected one.
pub fn resolve_device(udid: Option<&str>) -> Result<Device> {
    let mut conn = Connection::open()?;
    let devices = conn.list_devices()?;
    if devices.is_empty() {
        bail!("no iOS devices connected");
    }
    match udid {
        None => Ok(devices.into_iter().next().unwrap()),
        Some(u) => devices
            .into_iter()
            .find(|d| d.serial.eq_ignore_ascii_case(u))
            .ok_or_else(|| anyhow::anyhow!("device {u} not found")),
    }
}

/// Resolve a device and open a `DeviceSession` using the given mode.
///
/// In `Auto` mode the session picks RSD for iOS 17+ and falls back to
/// legacy lockdownd automatically.  Pass `ConnectionMode::Legacy` (or set
/// `IOS_LEGACY=1`) to always use the legacy path.
pub fn open_session(udid: Option<&str>, mode: ConnectionMode) -> Result<DeviceSession> {
    let device = resolve_device(udid)?;
    DeviceSession::open(device, mode).map_err(|e| anyhow::anyhow!("{e}"))
}
