use thiserror::Error;
use super::types::{ProtoError, ResultCode};

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("usbmuxd protocol error: {0}")]
    Proto(#[from] ProtoError),
    #[error("usbmuxd connect failed: {0}")]
    ConnectFailed(ResultCode),
    #[error("usbmuxd closed connection unexpectedly")]
    Closed,
    #[error("request failed: {0}")]
    RequestFailed(ResultCode),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("plist error: {0}")]
    Plist(#[from] plist::Error),
}
