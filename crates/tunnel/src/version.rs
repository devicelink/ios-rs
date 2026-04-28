use crate::Error;
use lockdown::LockdownSession;

/// Parsed iOS version triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IosVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl IosVersion {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        IosVersion { major, minor, patch }
    }

    /// True if this is iOS 17.4 or later (CoreDeviceProxy tunnel path).
    pub fn supports_core_device_proxy(&self) -> bool {
        *self >= IosVersion::new(17, 4, 0)
    }

    /// True if this is iOS 17.0 or later (RSD/RemoteXPC path, requires USB-Ethernet).
    pub fn supports_rsd(&self) -> bool {
        self.major >= 17
    }

    /// True if this device uses the legacy usbmux/lockdownd-only path.
    pub fn is_legacy(&self) -> bool {
        self.major < 17
    }
}

impl std::fmt::Display for IosVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Query the device's iOS version via lockdownd.
pub fn detect_version(device_id: u32) -> Result<IosVersion, Error> {
    let mut session = LockdownSession::connect(device_id)?;
    let val = session.get_value(None, "ProductVersion")?;
    let s = match &val {
        plist::Value::String(s) => s.clone(),
        _ => return Err(Error::Version("ProductVersion not a string".into())),
    };
    parse_version(&s)
}

pub fn parse_version(s: &str) -> Result<IosVersion, Error> {
    let parts: Vec<&str> = s.split('.').collect();
    let major = parts.first().and_then(|p| p.parse::<u32>().ok())
        .ok_or_else(|| Error::Version(format!("bad version: {s}")))?;
    let minor = parts.get(1).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
    Ok(IosVersion { major, minor, patch })
}
