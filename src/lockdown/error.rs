use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("usbmux error: {0}")]
    Usbmux(#[from] crate::usbmux::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("plist error: {0}")]
    Plist(#[from] plist::Error),
    #[error("lockdownd error: {0}")]
    Lockdown(String),
    #[error("pair record error: {0}")]
    PairRecord(String),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("AFC error: {0}")]
    Afc(String),
    #[error("connection closed unexpectedly")]
    Closed,
}
