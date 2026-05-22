use std::io::{Read, Write};
use std::net::TcpStream;

use super::codec::Codec;
use super::error::Error;
use super::socket::MuxSocket;
use super::types::{Device, Event, ResultCode};

pub struct Connection {
    socket: MuxSocket,
    codec:  Codec,
}

impl Connection {
    pub fn open() -> Result<Self, Error> {
        Ok(Connection { socket: MuxSocket::connect()?, codec: Codec::new() })
    }

    pub fn open_at(addr: std::net::SocketAddr) -> Result<Self, Error> {
        Ok(Connection {
            socket: MuxSocket::Tcp(TcpStream::connect(addr)?),
            codec:  Codec::new(),
        })
    }

    pub fn from_stream(stream: MuxSocket) -> Self {
        Connection { socket: stream, codec: Codec::new() }
    }

    pub fn list_devices(&mut self) -> Result<Vec<Device>, Error> {
        let tag = self.codec.list_devices();
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::DeviceList(devs)             => return Ok(devs),
                Event::RequestFailed { tag: t, .. } if t == tag => {
                    return Err(Error::RequestFailed(ResultCode::BadDevice))
                }
                _ => continue,
            }
        }
    }

    pub fn read_buid(&mut self) -> Result<String, Error> {
        let _tag = self.codec.read_buid();
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::Buid(b) => return Ok(b),
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
                _ => continue,
            }
        }
    }

    pub fn delete_pair_record(&mut self, udid: &str) -> Result<(), Error> {
        let _tag = self.codec.delete_pair_record(udid);
        self.flush()?;
        match self.next_event()? {
            Event::RequestFailed { .. } => Err(Error::RequestFailed(ResultCode::BadDevice)),
            _ => Ok(()),
        }
    }

    pub fn save_pair_record(&mut self, udid: &str, record: Vec<u8>) -> Result<(), Error> {
        let _tag = self.codec.save_pair_record(udid, record);
        self.flush()?;
        match self.next_event()? {
            Event::RequestFailed { .. } => Err(Error::RequestFailed(ResultCode::BadDevice)),
            _ => Ok(()),
        }
    }

    /// Open a forwarded TCP tunnel to `device_id:port`.
    /// Consumes the Connection — after a successful connect the socket
    /// is a raw pipe to the device service.
    pub fn open_tunnel(mut self, device_id: u32, port: u16) -> Result<MuxSocket, Error> {
        let tag = self.codec.connect(device_id, port);
        self.flush()?;
        loop {
            match self.next_event()? {
                Event::Connected { tag: t } if t == tag => return Ok(self.socket),
                Event::RequestFailed { code, .. } => return Err(Error::RequestFailed(code)),
                _ => continue,
            }
        }
    }

    pub fn listen(mut self) -> Result<Listener, Error> {
        let _tag = self.codec.listen();
        self.flush()?;
        Ok(Listener { inner: self })
    }

    pub(crate) fn next_event(&mut self) -> Result<Event, Error> {
        loop {
            if let Some(ev) = self.codec.poll_event() { return Ok(ev); }
            let mut buf = [0u8; 4096];
            let n = self.socket.read(&mut buf)?;
            if n == 0 { return Err(Error::ConnectionClosed); }
            self.codec.push_data(&buf[..n]);
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        while let Some(frame) = self.codec.poll_write() {
            self.socket.write_all(&frame)?;
        }
        self.socket.flush()?;
        Ok(())
    }
}

pub struct Listener {
    inner: Connection,
}

impl Listener {
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

