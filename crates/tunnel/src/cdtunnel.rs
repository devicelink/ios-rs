/// CDTunnel framing: `b"CDTunnel"` + `u16be` (body length) + JSON body.
///
/// After the handshake, raw IPv6 packets are forwarded on the same socket
/// (no CDTunnel framing wraps individual packets).
use std::io::{Read, Write};
use std::net::{Ipv6Addr, TcpStream};

use serde::Deserialize;

use crate::Error;

const MAGIC: &[u8] = b"CDTunnel";

// ── handshake types ──────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct ClientHandshake {
    #[serde(rename = "type")]
    kind: &'static str,
    mtu:  u16,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServerHandshake {
    pub server_address:  String,
    #[serde(rename = "serverRSDPort")]
    pub server_rsd_port: u16,
    pub client_parameters: ClientParams,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClientParams {
    pub address: String,
    pub mtu:     u16,
}

/// Fully resolved tunnel addressing and MTU after the handshake.
#[derive(Debug, Clone)]
pub struct TunnelParams {
    pub client_addr:     Ipv6Addr,
    pub server_addr:     Ipv6Addr,
    pub server_rsd_port: u16,
    pub mtu:             u16,
}

/// A socket with CDTunnel framing, backed by a plain `TcpStream`.
pub struct CdTunnelConn {
    stream: TcpStream,
    pub params: TunnelParams,
}

impl CdTunnelConn {
    /// Perform the CDTunnel JSON handshake on a plain TCP socket.
    pub fn handshake(mut stream: TcpStream) -> Result<Self, Error> {
        let params = do_handshake(&mut stream)?;
        Ok(CdTunnelConn { stream, params })
    }

    /// Perform the CDTunnel JSON handshake on any `Read + Write` stream and
    /// return only the `TunnelParams`.  Used when the caller needs to keep
    /// ownership of the stream (e.g. TLS-wrapped sockets).
    pub fn handshake_params<S: Read + Write>(stream: &mut S) -> Result<TunnelParams, Error> {
        do_handshake(stream)
    }

    /// Send a raw IPv6 packet to the device.
    pub fn send_ipv6_packet(&mut self, packet: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(packet)
    }

    /// Receive the next raw IPv6 packet from the device (blocking).
    pub fn recv_ipv6_packet(&mut self) -> std::io::Result<Vec<u8>> {
        recv_ipv6(&mut self.stream)
    }

    /// Clone the underlying TCP stream (for the reader thread).
    pub fn try_clone_stream(&self) -> std::io::Result<TcpStream> {
        self.stream.try_clone()
    }

    pub fn set_nonblocking(&self, nb: bool) -> std::io::Result<()> {
        self.stream.set_nonblocking(nb)
    }

    pub fn try_recv_ipv6_packet(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        match self.recv_ipv6_packet() {
            Ok(p)                                                 => Ok(Some(p)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut   => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ── shared framing helpers ────────────────────────────────────────────────────

fn do_handshake<S: Read + Write>(stream: &mut S) -> Result<TunnelParams, Error> {
    let req  = ClientHandshake { kind: "clientHandshakeRequest", mtu: 1280 };
    send_frame(stream, &serde_json::to_vec(&req)?)?;

    let body = recv_frame(stream)?;
    let resp: ServerHandshake = serde_json::from_slice(&body)?;

    let client_addr = resp.client_parameters.address.parse::<Ipv6Addr>()
        .map_err(|e| Error::Protocol(format!("bad client IPv6: {e}")))?;
    let server_addr = resp.server_address.parse::<Ipv6Addr>()
        .map_err(|e| Error::Protocol(format!("bad server IPv6: {e}")))?;

    Ok(TunnelParams {
        client_addr,
        server_addr,
        server_rsd_port: resp.server_rsd_port,
        mtu: resp.client_parameters.mtu,
    })
}

fn send_frame<S: Write>(stream: &mut S, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(MAGIC)?;
    stream.write_all(&(body.len() as u16).to_be_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn recv_frame<S: Read>(stream: &mut S) -> Result<Vec<u8>, Error> {
    let mut magic = [0u8; 8];
    read_exact(stream, &mut magic)?;
    if magic != MAGIC {
        return Err(Error::Protocol(format!("bad CDTunnel magic: {:?}", &magic)));
    }
    let mut len_buf = [0u8; 2];
    read_exact(stream, &mut len_buf)?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    read_exact(stream, &mut body)?;
    Ok(body)
}

/// Read a full raw IPv6 packet from any `Read` source.
pub fn recv_ipv6<S: Read>(s: &mut S) -> std::io::Result<Vec<u8>> {
    let mut hdr = [0u8; 40];
    read_exact(s, &mut hdr)?;
    let payload = u16::from_be_bytes([hdr[4], hdr[5]]) as usize;
    let mut pkt = vec![0u8; 40 + payload];
    pkt[..40].copy_from_slice(&hdr);
    read_exact(s, &mut pkt[40..])?;
    Ok(pkt)
}

fn read_exact<S: Read>(s: &mut S, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = s.read(&mut buf[done..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "CDTunnel: connection closed",
            ));
        }
        done += n;
    }
    Ok(())
}
