use thiserror::Error;
use usbmux_proto::ResultCode;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("usbmuxd protocol error: {0}")]
    Proto(#[from] usbmux_proto::ProtoError),
    #[error("usbmuxd connect failed: {0}")]
    ConnectFailed(ResultCode),
    #[error("usbmuxd closed connection unexpectedly")]
    Closed,
    #[error("request failed: {0}")]
    RequestFailed(ResultCode),
}
