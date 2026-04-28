mod codec;
mod connection;
mod error;
mod plist_msg;
mod socket;
mod types;

pub use connection::{Connection, Listener};
pub use error::Error;
pub use socket::MuxSocket;
pub use types::{ConnectionType, Device, Event, ResultCode};

#[cfg(feature = "sim")]
pub mod sim;
