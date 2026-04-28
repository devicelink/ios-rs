/// Which connection path to use when talking to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionMode {
    /// Detect by iOS version: RSD if supported (iOS 17+), legacy otherwise.
    /// Falls back to legacy if RSD tunnel cannot be established.
    #[default]
    Auto,
    /// Always use usbmux → lockdownd regardless of iOS version.
    Legacy,
    /// Force RSD path; return an error if the device doesn't support it or
    /// the tunnel cannot be established.
    Rsd,
}

impl ConnectionMode {
    /// Read from the `IOS_LEGACY` environment variable.
    /// `IOS_LEGACY=1` forces [`ConnectionMode::Legacy`].
    pub fn from_env() -> Self {
        match std::env::var("IOS_LEGACY").as_deref() {
            Ok("1") | Ok("true") | Ok("yes") => ConnectionMode::Legacy,
            _ => ConnectionMode::Auto,
        }
    }

    /// Apply a `--legacy` CLI flag on top of the current mode.
    pub fn with_legacy_flag(self, legacy: bool) -> Self {
        if legacy { ConnectionMode::Legacy } else { self }
    }

    pub fn is_legacy(self) -> bool { self == ConnectionMode::Legacy }
    pub fn is_rsd(self)    -> bool { self == ConnectionMode::Rsd }
}

impl std::fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionMode::Auto   => write!(f, "auto"),
            ConnectionMode::Legacy => write!(f, "legacy"),
            ConnectionMode::Rsd   => write!(f, "rsd"),
        }
    }
}
