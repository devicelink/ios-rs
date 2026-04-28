//! Fake usbmuxd server for integration testing.
//!
//! Speaks the real plist-over-length-prefixed-header protocol so the actual
//! `usbmux::Connection` client code is exercised unchanged.
//!
//! # Usage
//!
//! ```
//! use usbmux_sim::{UsbmuxSim, SimDevice};
//! use usbmux::Connection;
//!
//! let sim = UsbmuxSim::start(vec![
//!     SimDevice::usb("ABC123DEF456"),
//! ]);
//! let mut conn = Connection::open_at(sim.addr()).unwrap();
//! let devices  = conn.list_devices().unwrap();
//! assert_eq!(devices.len(), 1);
//! assert_eq!(devices[0].serial, "ABC123DEF456");
//! ```

mod framing;
mod handler;
mod server;

pub use server::{SimDevice, UsbmuxSim};
