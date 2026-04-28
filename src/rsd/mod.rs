use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

use crate::remotexpc::RemoteXpcConn;
use crate::xpc::Value;
use thiserror::Error;


/// Hard-coded RSD port — the device always listens here for initial discovery.
pub const RSD_PORT: u16 = 58783;

#[derive(Debug, Error)]
pub enum Error {
    #[error("remotexpc: {0}")]
    Xpc(#[from] crate::remotexpc::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected RSD response: {0}")]
    Protocol(String),
}

/// Port + metadata for a single service in the RSD catalogue.
#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub port:             u16,
    pub uses_remote_xpc:  bool,
    pub properties:       HashMap<String, Value>,
}

/// Peer information returned by the RSD handshake.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub udid:            String,
    pub product_type:    String,
    pub os_version:      String,
    pub build_version:   String,
}

/// A live RSD connection.
///
/// Created by connecting via RemoteXPC to the device's IPv6 address on
/// [`RSD_PORT`] (initial discovery) or the port returned by the CDTunnel
/// handshake (inside-tunnel RSD).
pub struct RsdClient {
    #[allow(dead_code)] // kept alive to hold the TCP connection open
    conn:      RemoteXpcConn,
    peer_info: PeerInfo,
    services:  HashMap<String, ServiceEntry>,
}

impl RsdClient {
    /// Connect to `addr:port` and receive the RSD Handshake message.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        let conn = RemoteXpcConn::connect(addr)?;
        Self::from_conn(conn)
    }

    /// Accept an already-open `UnixStream` (e.g. from smoltcp) and do the RSD handshake.
    ///
    /// Routes through a loopback TCP relay because `RemoteXpcConn` uses `TcpStream`.
    pub fn connect_stream(stream: std::os::unix::net::UnixStream) -> Result<Self, Error> {
        use std::net::TcpListener;
        let listener   = TcpListener::bind("127.0.0.1:0").map_err(crate::remotexpc::Error::Io)?;
        let local_addr = listener.local_addr().map_err(crate::remotexpc::Error::Io)?;

        std::thread::spawn(move || {
            let Ok((tcp, _)) = listener.accept() else { return };
            // Two directions: unix→tcp and tcp→unix
            let mut uni_r  = stream.try_clone().unwrap();
            let mut uni_w  = stream;
            let mut tcp_w  = tcp.try_clone().unwrap();
            let mut tcp_r  = tcp;
            let t1 = std::thread::spawn(move || copy_half(&mut uni_r, &mut tcp_w));
            let t2 = std::thread::spawn(move || copy_half(&mut tcp_r, &mut uni_w));
            let _ = (t1.join(), t2.join());
        });

        let conn = RemoteXpcConn::connect(local_addr)?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: RemoteXpcConn) -> Result<Self, Error> {
        // The device sends a Handshake on the SC stream, possibly after some init frames.
        // Loop until we find a message with a "Properties" key.
        let (peer_info, services) = loop {
            let msg = conn.receive()?;
            let Some(body) = msg.body else { continue };
            match parse_handshake(&body) {
                Ok(result) => break result,
                Err(_)     => continue,
            }
        };

        Ok(RsdClient { conn, peer_info, services })
    }

    pub fn peer_info(&self) -> &PeerInfo { &self.peer_info }
    pub fn services(&self) -> &HashMap<String, ServiceEntry> { &self.services }

    /// Look up a service by name.
    pub fn service(&self, name: &str) -> Option<&ServiceEntry> {
        self.services.get(name)
    }

    /// Send an `RSDCheckin` plist on a raw TCP connection to a shim service.
    ///
    /// Shim services (legacy lockdown services wrapped for RemoteXPC) expect
    /// this plist handshake before starting to speak their own protocol.
    pub fn rsd_checkin(stream: &mut TcpStream) -> Result<(), Error> {
        use std::io::Write;
        let plist = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
            <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
            \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
            <plist version=\"1.0\"><dict>\
            <key>Label</key><string>devicelink</string>\
            <key>ProtocolVersion</key><string>2</string>\
            <key>Request</key><string>RSDCheckin</string>\
            </dict></plist>\n";

        let len = plist.len() as u32;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(plist)?;
        stream.flush()?;
        Ok(())
    }
}

// ── parse handshake ──────────────────────────────────────────────────────────

fn parse_handshake(body: &Value) -> Result<(PeerInfo, HashMap<String, ServiceEntry>), Error> {
    let dict = body.as_dict()
        .ok_or_else(|| Error::Protocol("Handshake body not a dict".into()))?;

    let props = dict.get("Properties")
        .and_then(|v| v.as_dict())
        .ok_or_else(|| Error::Protocol("no Properties in Handshake".into()))?;

    let peer_info = PeerInfo {
        udid:          str_or(props, "UniqueDeviceID"),
        product_type:  str_or(props, "ProductType"),
        os_version:    str_or(props, "OSVersion"),
        build_version: str_or(props, "BuildVersion"),
    };

    let mut services = HashMap::new();

    if let Some(svc_val) = dict.get("Services") {
        if let Some(svc_map) = svc_val.as_dict() {
            for (name, entry_val) in svc_map {
                if let Some(entry) = parse_service_entry(entry_val) {
                    services.insert(name.clone(), entry);
                }
            }
        }
    }

    Ok((peer_info, services))
}

fn parse_service_entry(val: &Value) -> Option<ServiceEntry> {
    let dict = val.as_dict()?;

    // Port may be a string ("58783") or an integer
    let port = dict.get("Port")
        .and_then(|v| match v {
            Value::String(s) => s.parse::<u16>().ok(),
            other            => other.as_u64().map(|n| n as u16),
        })?;

    let uses_remote_xpc = dict.get("Properties")
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("UsesRemoteXPC"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let properties = dict.get("Properties")
        .and_then(|v| v.as_dict())
        .cloned()
        .unwrap_or_default();

    Some(ServiceEntry { port, uses_remote_xpc, properties })
}

fn copy_half(src: &mut dyn Read, dst: &mut dyn Write) {
    let mut buf = [0u8; 16384];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => if dst.write_all(&buf[..n]).is_err() { break },
        }
    }
}

fn str_or(d: &HashMap<String, Value>, key: &str) -> String {
    d.get(key).and_then(|v| v.as_str()).map(|s| s.to_owned()).unwrap_or_default()
}
