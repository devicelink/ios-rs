use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("usbmux: {0}")]
    Usbmux(#[from] usbmux::Error),
    #[error("lockdown: {0}")]
    Lockdown(#[from] lockdown::Error),
    #[error("rsd: {0}")]
    Rsd(#[from] rsd::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CDTunnel protocol: {0}")]
    Protocol(String),
    #[error("no device connected")]
    NoDevice,
    #[error("iOS version parse error: {0}")]
    Version(String),
}
