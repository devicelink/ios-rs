use std::io::{Read, Write};

use usbmux_proto::{Codec, Device, Event};

use crate::error::Error;
use crate::socket::MuxSocket;

/// A connection to usbmuxd.
///
/// Each method consumes a request/response exchange.
/// After [`Connection::open_tunnel`] the underlying socket is extracted and
/// returned as a raw [`MuxSocket`] for the caller to use as a transparent
/// byte pipe.
pub struct Connection {
    codec:  Codec,
    socket: MuxSocket,
}

impl Connection {
    /// Connect to the local usbmuxd socket/port.
    pub fn open() -> Result<Self, Error> {
        let socket = MuxSocket::connect()?;
        Ok(Connection { codec: Codec::new(), socket })
    }

    /// Connect to a specific TCP address — used in tests to target a simulator.
    pub fn open_at(addr: impl std::net::ToSocketAddrs) -> Result<Self, Error> {
        use std::net::TcpStream;
        let tcp = TcpStream::connect(addr)?;
        Ok(Connection { codec: Codec::new(), socket: MuxSocket::Tcp(tcp) })
    }

    /// Use a caller-provided stream (e.g. a Wasm WIT conn resource).
    /// The stream must already be connected to usbmuxd.
    pub fn from_stream<S: crate::socket::RwStream + 'static>(stream: S) -> Self {
        Connection { codec: Codec::new(), socket: MuxSocket::external(stream) }
    }

    // ── public API ───────────────────────────────────────────────────────────

    pub fn list_devices(&mut self) -> Result<Vec<Device>, Error> {
        let _tag = self.codec.list_devices();
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::DeviceList(devices) => return Ok(devices),
                Event::RequestFailed { code, .. } => return Err(Error::RequestFailed(code)),
                _ => continue,
            }
        }
    }

    pub fn read_buid(&mut self) -> Result<String, Error> {
        let _tag = self.codec.read_buid();
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::Buid(buid) => return Ok(buid),
                Event::RequestFailed { code, .. } => return Err(Error::RequestFailed(code)),
                _ => continue,
            }
        }
    }

    pub fn read_pair_record(&mut self, udid: &str) -> Result<Vec<u8>, Error> {
        let _tag = self.codec.read_pair_record(udid);
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::PairRecord { record, .. } => return Ok(record),
                Event::RequestFailed { code, .. } => return Err(Error::RequestFailed(code)),
                _ => continue,
            }
        }
    }

    /// Listen for device attach/detach events. Returns an iterator-like
    /// wrapper that yields events until the socket closes.
    pub fn listen(mut self) -> Result<Listener, Error> {
        let _tag = self.codec.listen();
        self.flush()?;
        // Don't wait for Result:OK here — the codec drops plain OKs without
        // emitting an event, so next_event() would block until the first
        // DeviceAttached arrives and consume it.  Errors surface on the first
        // Listener::next() call instead.
        Ok(Listener { inner: self })
    }

    /// Open a transparent tunnel to a service port on a device.
    ///
    /// On success the underlying socket is consumed and returned. All
    /// subsequent bytes on that socket are forwarded directly to/from the
    /// device service — the codec is no longer in the path.
    pub fn open_tunnel(mut self, device_id: u32, port: u16) -> Result<MuxSocket, Error> {
        let tag = self.codec.connect(device_id, port);
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::Connected { tag: t } if t == tag => return Ok(self.socket),
                Event::RequestFailed { code, .. } => return Err(Error::ConnectFailed(code)),
                _ => continue,
            }
        }
    }

    // ── internals ────────────────────────────────────────────────────────────

    pub(crate) fn flush(&mut self) -> Result<(), Error> {
        while let Some(frame) = self.codec.poll_write() {
            self.socket.write_all(&frame)?;
        }
        Ok(())
    }

    pub(crate) fn next_event(&mut self) -> Result<Event, Error> {
        loop {
            if let Some(ev) = self.codec.poll_event() {
                return Ok(ev);
            }
            let mut buf = [0u8; 8192];
            let n = self.socket.read(&mut buf)?;
            if n == 0 {
                return Err(Error::Closed);
            }
            self.codec.push_data(&buf[..n]);
        }
    }
}

// ── Listener ──────────────────────────────────────────────────────────────────

/// Wraps a listening Connection and yields attach/detach events.
pub struct Listener {
    inner: Connection,
}

impl Listener {
    /// Block until the next device attach or detach event.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Event, Error> {
        loop {
            match self.inner.next_event()? {
                ev @ (Event::DeviceAttached(_) | Event::DeviceDetached { .. }) => return Ok(ev),
                Event::RequestFailed { code, .. } => return Err(Error::RequestFailed(code)),
                _ => continue,
            }
        }
    }
}
