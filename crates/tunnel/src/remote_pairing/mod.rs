mod credentials;
mod framing;
mod session;
mod tlv8;

pub use credentials::RemotePairingRecord;
pub use session::{ReadWrite, RemotePairingSession};


use crate::cdtunnel::CdTunnelConn;
use crate::error::Error;
use crate::smoltcp_stack::SmoltcpTunnel;

/// Perform the full RemotePairing handshake on a CoreDeviceProxy service socket,
/// then issue `createListener`, do the CDTunnel handshake, and spin up the
/// smoltcp userspace IP stack.  Returns a `SmoltcpTunnel` ready for TCP connections.
pub fn establish_tunnel(
    service_stream: impl ReadWrite + 'static,
    udid:           &str,
) -> Result<SmoltcpTunnel, Error> {
    let record = RemotePairingRecord::load(udid)
        .unwrap_or_else(|| RemotePairingRecord::new_identity(udid));

    let (mut session, _updated) =
        RemotePairingSession::open(service_stream, record, udid)?;

    // Request a TCP tunnel listener on the device
    let resp = session.encrypted_request(&serde_json::json!({
        "request": {
            "_0": {
                "createListener": {
                    "key": "",
                    "peerConnectionsInfo": [{
                        "owningPID": std::process::id(),
                        "owningProcessName": "devicelink",
                    }],
                    "transportProtocolType": "tcp",
                }
            }
        }
    }))?;

    let port = resp["createListener"]["port"]
        .as_u64()
        .ok_or_else(|| Error::Protocol(format!(
            "createListener response missing port: {resp}"
        )))? as u16;

    eprintln!("CDTunnel listener ready on device port {port}");

    // Open a new usbmux tunnel to the CDTunnel listener port
    let tunnel_socket = {
        let devices = usbmux::Connection::open()?.list_devices()?;
        let dev = devices.into_iter()
            .find(|d| d.serial.eq_ignore_ascii_case(udid))
            .ok_or_else(|| Error::Protocol(format!("device {udid} not found for CDTunnel")))?;
        usbmux::Connection::open()?.open_tunnel(dev.device_id, port)?
    };

    let tcp = match tunnel_socket {
        usbmux::MuxSocket::Tcp(s) => s,
        #[cfg(unix)]
        usbmux::MuxSocket::Unix(_) => return Err(Error::Protocol(
            "CDTunnel requires TCP; set USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015".into()
        )),
        usbmux::MuxSocket::External(_) => return Err(Error::Protocol(
            "CDTunnel requires TCP socket".into()
        )),
    };

    eprintln!("Performing CDTunnel handshake…");
    let cdtunnel = CdTunnelConn::handshake(tcp)?;
    eprintln!("CDTunnel up — client={} server={} rsd_port={}",
        cdtunnel.params.client_addr, cdtunnel.params.server_addr, cdtunnel.params.server_rsd_port);

    // Spin up the smoltcp userspace IPv6 stack
    SmoltcpTunnel::new(cdtunnel).map_err(|e| Error::Protocol(format!("smoltcp init: {e}")))
}
