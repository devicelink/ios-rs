pub mod apps;
pub mod devices;
pub mod info;
pub mod relay;
pub mod rsd;
pub mod services;
pub mod version;
pub mod watch;

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
