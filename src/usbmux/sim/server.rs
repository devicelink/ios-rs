use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use super::handler;

/// A service handler: receives the raw socket after the Connect handshake succeeds.
/// The function should read/write the service protocol and then return.
pub type ServiceFn = Arc<dyn Fn(&mut TcpStream) + Send + Sync + 'static>;

/// A simulated iOS device.
#[derive(Clone)]
pub struct SimDevice {
    pub serial:          String,
    pub connection_type: String,
    pub product_id:      u16,
    /// Registered service handlers: (port, handler).
    pub services:        Vec<(u16, ServiceFn)>,
}

impl SimDevice {
    /// A USB-connected device with the given UDID.
    pub fn usb(serial: impl Into<String>) -> Self {
        SimDevice {
            serial:          serial.into(),
            connection_type: "USB".into(),
            product_id:      0x12a8,
            services:        Vec::new(),
        }
    }

    /// A Network-connected device.
    pub fn network(serial: impl Into<String>) -> Self {
        SimDevice {
            serial:          serial.into(),
            connection_type: "Network".into(),
            product_id:      0x12a8,
            services:        Vec::new(),
        }
    }

    /// Register a handler for a service port.
    ///
    /// The handler receives the raw socket after the Connect handshake succeeds.
    /// Write plist responses as a real service would.
    pub fn with_service<F>(mut self, port: u16, f: F) -> Self
    where
        F: Fn(&mut TcpStream) + Send + Sync + 'static,
    {
        self.services.push((port, Arc::new(f)));
        self
    }
}

/// A running fake usbmuxd server.
///
/// Dropped when the handle goes out of scope (server thread keeps running until
/// all client connections close, which happens when the test drops its
/// `Connection` handles).
pub struct UsbmuxSim {
    addr: SocketAddr,
    /// Kept alive so the listener thread can call `accept`.
    _listener_thread: thread::JoinHandle<()>,
}

impl UsbmuxSim {
    /// Start a simulator with the given device set. Binds to a random local port.
    pub fn start(devices: Vec<SimDevice>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("sim: bind");
        let addr = listener.local_addr().expect("sim: local_addr");
        let devices = Arc::new(devices);

        let handle = thread::Builder::new()
            .name("usbmux-sim".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let devs = Arc::clone(&devices);
                    thread::spawn(move || handler::handle(stream, &devs));
                }
            })
            .expect("sim: spawn");

        UsbmuxSim { addr, _listener_thread: handle }
    }

    /// The address clients should connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}
