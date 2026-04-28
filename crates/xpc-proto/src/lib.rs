mod codec;
mod value;

pub use codec::{decode_message, encode_message};
pub use value::Value;

/// XPC wrapper magic (all messages)
pub const WRAPPER_MAGIC: u32 = 0x29b0_0b92;
/// XPC payload magic (when body is present)
pub const PAYLOAD_MAGIC: u32 = 0x4213_3742;
pub const PAYLOAD_VERSION: u32 = 0x0000_0005;

/// XPC message flags
pub mod flags {
    pub const ALWAYS_SET: u32      = 0x0000_0001;
    pub const DATA_PRESENT: u32    = 0x0000_0100;
    pub const WANTING_REPLY: u32   = 0x0001_0000;
    pub const REPLY: u32           = 0x0002_0000;
    pub const INIT_HANDSHAKE: u32  = 0x0040_0000;
}

/// XPC wire message (wrapper + optional body).
#[derive(Debug, Clone)]
pub struct Message {
    pub flags:  u32,
    pub msg_id: u64,
    /// Present when `flags & DATA_PRESENT != 0`
    pub body:   Option<Value>,
}

impl Message {
    /// Construct a data-carrying message.
    pub fn with_body(msg_id: u64, body: Value) -> Self {
        Message {
            flags: flags::ALWAYS_SET | flags::DATA_PRESENT | flags::WANTING_REPLY,
            msg_id,
            body: Some(body),
        }
    }

    /// Construct a reply to the given message id.
    pub fn reply(msg_id: u64, body: Value) -> Self {
        Message {
            flags: flags::ALWAYS_SET | flags::DATA_PRESENT | flags::REPLY,
            msg_id,
            body: Some(body),
        }
    }

    /// Handshake init frames (no body).
    pub fn init(flags: u32) -> Self {
        Message { flags, msg_id: 0, body: None }
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum XpcError {
    #[error("buffer too short: need {need}, got {got}")]
    TooShort { need: usize, got: usize },
    #[error("bad magic: {0:#010x}")]
    BadMagic(u32),
    #[error("unknown XPC type tag: {0:#010x}")]
    UnknownType(u32),
    #[error("invalid UTF-8 in XPC string")]
    InvalidUtf8,
}
