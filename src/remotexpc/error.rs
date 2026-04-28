use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("XPC codec error: {0}")]
    Xpc(#[from] crate::xpc::XpcError),
    #[error("HTTP/2 protocol error: {0}")]
    H2(String),
    #[error("connection closed")]
    Closed,
    #[error("unexpected frame type {0:#04x}")]
    UnexpectedFrame(u8),
}
