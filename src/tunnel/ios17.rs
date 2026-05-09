/// iOS 17+ tunnel management.
use std::io::BufReader;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pemfile::{certs, private_key};

use crate::lockdown::{LockdownSession, PairRecord};
use crate::rsd::RsdClient;
use crate::usbmux::Connection as MuxConn;

use super::cdtunnel::CdTunnelConn;
use super::error::Error;
use super::smoltcp_stack::SmoltcpTunnel;
use super::version::IosVersion;

const CORE_DEVICE_PROXY: &str = "com.apple.internal.devicecompute.CoreDeviceProxy";

/// Timeout for the CDTunnel JSON handshake (first byte from device).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Read timeout used in the smoltcp unified loop (short, for polling).
const POLL_READ_TIMEOUT: Duration = Duration::from_millis(2);

/// An active iOS 17+ tunnel with a live smoltcp IPv6 stack.
pub struct Ios17Tunnel {
    pub stack:   SmoltcpTunnel,
    pub version: IosVersion,
}

impl Ios17Tunnel {
    pub fn connect(device_id: u32) -> Result<Self, Error> {
        let version = super::version::detect_version(device_id)?;
        if version.supports_core_device_proxy() {
            Self::connect_via_lockdown_udid(device_id, None, version)
        } else if version.supports_rsd() {
            Err(Error::Protocol(format!(
                "iOS {version}: USB-Ethernet tunnel not yet implemented"
            )))
        } else {
            Err(Error::Protocol(format!(
                "iOS {version} uses legacy lockdown-only path"
            )))
        }
    }

    pub fn connect_via_lockdown(device_id: u32, version: IosVersion) -> Result<Self, Error> {
        Self::connect_via_lockdown_udid(device_id, None, version)
    }

    pub fn connect_via_lockdown_udid(
        device_id: u32,
        udid:      Option<&str>,
        version:   IosVersion,
    ) -> Result<Self, Error> {
        let owned_udid;
        let udid: &str = match udid {
            Some(u) => u,
            None => {
                let mut s = LockdownSession::connect(device_id)?;
                owned_udid = s.get_value(None, "UniqueDeviceID")
                    .ok()
                    .and_then(|v| if let plist::Value::String(s) = v { Some(s) } else { None })
                    .unwrap_or_default();
                &owned_udid
            }
        };

        // 1. Open a paired lockdownd session and start CoreDeviceProxy
        let mut session = LockdownSession::open_paired(device_id, udid)?;
        let svc         = session.start_service(CORE_DEVICE_PROXY)?;

        // 2. Open the raw usbmux tunnel.
        // The CDTunnel handshake and TLS layer need a TcpStream (for set_read_timeout
        // and rustls::StreamOwned).  When usbmuxd gives us a Unix socket (the common
        // macOS case via /var/run/usbmuxd) we bridge it through a loopback TCP relay —
        // the same pattern used in the DTX and RSD connection helpers.
        let tcp: TcpStream = {
            let socket = MuxConn::open()?.open_tunnel(device_id, svc.port)?;
            match socket {
                crate::usbmux::MuxSocket::Tcp(s) => s,
                #[cfg(unix)]
                crate::usbmux::MuxSocket::Unix(unix) => unix_to_tcp(unix)
                    .map_err(|e| Error::Protocol(format!("Unix→TCP relay: {e}")))?,
                crate::usbmux::MuxSocket::External(_) => return Err(Error::Protocol(
                    "CoreDeviceProxy requires a socket stream".into()
                )),
            }
        };

        // 3. Perform the CDTunnel handshake (optionally over TLS).
        //    go-ios sends CDTunnel JSON directly over TLS without any
        //    RemotePairing wrapper. Set a 10 s timeout for the handshake,
        //    then switch to a short poll timeout for the smoltcp loop.
        let stack = if svc.enable_service_ssl {
            let pair = PairRecord::read_from_usbmuxd(udid)?;
            let tls  = tls_wrap(tcp, &pair)?;
            // Short timeout so the unified loop doesn't block on reads.
            tls.get_ref().set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
            cdtunnel_over_tls(tls, udid, &mut session)?
        } else {
            tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
            cdtunnel_over_tcp(tcp)?
        };

        drop(session);
        Ok(Ios17Tunnel { stack, version })
    }

    /// Connect to the RSD service through the smoltcp stack.
    pub fn connect_rsd(&self) -> Result<RsdClient, Error> {
        let stream = self.stack.connect(
            self.stack.params.server_addr,
            self.stack.params.server_rsd_port,
        )?;
        RsdClient::connect_stream(stream)
            .map_err(|e| Error::Protocol(format!("RSD connect: {e}")))
    }
}

// ── CDTunnel handshake paths ──────────────────────────────────────────────────

/// CDTunnel handshake over a plain TCP socket.
fn cdtunnel_over_tcp(tcp: TcpStream) -> Result<SmoltcpTunnel, Error> {
    eprintln!("CDTunnel: attempting handshake over plain TCP…");
    let cdtunnel = CdTunnelConn::handshake(tcp)?;
    eprintln!("CDTunnel up — client={} server={} rsd_port={}",
        cdtunnel.params.client_addr,
        cdtunnel.params.server_addr,
        cdtunnel.params.server_rsd_port,
    );
    SmoltcpTunnel::new(cdtunnel)
}

/// CDTunnel handshake over a TLS-wrapped TCP socket.
/// After the handshake the stream is handed to the smoltcp unified loop.
fn cdtunnel_over_tls(
    mut tls: StreamOwned<ClientConnection, TcpStream>,
    _udid:   &str,
    _sess:   &mut LockdownSession,
) -> Result<SmoltcpTunnel, Error> {
    eprintln!("CDTunnel: attempting handshake over TLS…");
    let params = CdTunnelConn::handshake_params(&mut tls)?;
    eprintln!("CDTunnel up — client={} server={} rsd_port={}",
        params.client_addr, params.server_addr, params.server_rsd_port);

    // Switch to short read timeout for the smoltcp poll loop.
    tls.get_ref().set_read_timeout(Some(POLL_READ_TIMEOUT))?;
    SmoltcpTunnel::new_stream(tls, params)
}

// ── TLS helper ────────────────────────────────────────────────────────────────

fn tls_wrap(
    plain: TcpStream,
    pair:  &PairRecord,
) -> Result<StreamOwned<ClientConnection, TcpStream>, Error> {
    let config = build_tls_config(pair)?;
    let server_name = ServerName::try_from("localhost")
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut conn = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| Error::Protocol(format!("TLS init: {e}")))?;
    let mut sock = plain;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)
            .map_err(|e| Error::Protocol(format!("service TLS handshake: {e}")))?;
    }
    Ok(StreamOwned::new(conn, sock))
}

fn build_tls_config(pair: &PairRecord) -> Result<ClientConfig, Error> {
    let cert_chain: Vec<CertificateDer<'static>> = {
        let mut r = BufReader::new(pair.host_certificate.as_slice());
        certs(&mut r).collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::Protocol(format!("cert: {e}")))?
    };
    let key = {
        let mut r = BufReader::new(pair.host_private_key.as_slice());
        private_key(&mut r)
            .map_err(|e| Error::Protocol(format!("key: {e}")))?
            .ok_or_else(|| Error::Protocol("no private key".into()))?
    };
    ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_client_auth_cert(cert_chain, key)
        .map_err(|e| Error::Protocol(e.to_string()))
}

/// Bridge a Unix socket to a loopback TCP connection.
///
/// The CDTunnel and TLS layers need a `TcpStream` (`set_read_timeout`, `rustls`).
/// When usbmuxd uses its default Unix socket at `/var/run/usbmuxd`, the tunnel
/// socket comes back as `UnixStream`.  We spin up a loopback relay so the rest
/// of the code sees a plain `TcpStream` — zero protocol change, just socket type.
#[cfg(unix)]
fn unix_to_tcp(unix: std::os::unix::net::UnixStream) -> std::io::Result<TcpStream> {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr     = listener.local_addr()?;
    let client   = TcpStream::connect(addr)?;
    std::thread::spawn(move || {
        if let Ok((server, _)) = listener.accept() {
            let mut uni_r = unix.try_clone().unwrap();
            let mut uni_w = unix;
            let mut tcp_w = server.try_clone().unwrap();
            let mut tcp_r = server;
            let t1 = std::thread::spawn(move || { std::io::copy(&mut uni_r, &mut tcp_w).ok(); });
            let t2 = std::thread::spawn(move || { std::io::copy(&mut tcp_r, &mut uni_w).ok(); });
            let _ = (t1.join(), t2.join());
        }
    });
    Ok(client)
}

#[derive(Debug)]
struct AcceptAny;
impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> { Ok(ServerCertVerified::assertion()) }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256, SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512, SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,   SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::ECDSA_NISTP384_SHA384,
        ]
    }
}
