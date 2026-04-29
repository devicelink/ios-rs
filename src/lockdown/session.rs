use std::io::{Read, Write};
use std::sync::Arc;

use plist::Value;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use crate::usbmux::{Connection as MuxConn, MuxSocket};

use super::error::Error;
use super::pair_record::PairRecord;
use super::types::{DeviceInfo, ServiceInfo};

// ── stream abstraction ────────────────────────────────────────────────────────

/// Lockdown transport: starts as plain usbmux tunnel, upgrades to TLS after
/// `StartSession` if `EnableSessionSSL` is true.
enum Stream {
    Plain(MuxSocket),
    Tls(Box<StreamOwned<ClientConnection, MuxSocket>>),
    /// Transient state during TLS upgrade — never visible to callers.
    Upgrading,
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s)    => s.read(buf),
            Stream::Tls(s)      => s.read(buf),
            Stream::Upgrading   => unreachable!("read during TLS upgrade"),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s)    => s.write(buf),
            Stream::Tls(s)      => s.write(buf),
            Stream::Upgrading   => unreachable!("write during TLS upgrade"),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s)    => s.flush(),
            Stream::Tls(s)      => s.flush(),
            Stream::Upgrading   => unreachable!("flush during TLS upgrade"),
        }
    }
}

// ── TLS verifier ──────────────────────────────────────────────────────────────

/// Accepts any server certificate.
///
/// lockdownd uses a self-signed cert from the pair record; real authentication
/// is that the device accepts our client certificate (also from the pair record).
#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self, _end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>, _ocsp_response: &[u8], _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _message: &[u8], _cert: &CertificateDer<'_>, _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
        ]
    }
}

// ── wire types ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartSessionReq<'a> {
    request:     &'a str,
    #[serde(rename = "HostID")]
    host_id:     &'a str,
    #[serde(rename = "SystemBUID")]
    system_buid: &'a str,
    label:       &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StopSessionReq<'a> {
    request:    &'a str,
    #[serde(rename = "SessionID")]
    session_id: &'a str,
    label:      &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Request<'a> {
    request:   &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain:    Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key:       Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label:     Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service:   Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_escrow_bag: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct BaseResponse {
    error:  Option<String>,
    value:  Option<plist::Value>,
    port:   Option<u16>,
    #[serde(rename = "EnableServiceSSL")]
    enable_service_ssl: Option<bool>,
    // StartSession fields
    #[serde(rename = "SessionID")]
    session_id:         Option<String>,
    #[serde(rename = "EnableSessionSSL")]
    enable_session_ssl: Option<bool>,
}

// ── framing ───────────────────────────────────────────────────────────────────

fn send_plist<W: Write>(w: &mut W, value: &impl Serialize) -> Result<(), Error> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, value)?;
    let len = body.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

fn recv_plist<R: Read>(r: &mut R) -> Result<plist::Value, Error> {
    let mut len_buf = [0u8; 4];
    read_exact(r, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    read_exact(r, &mut body)?;
    Ok(plist::from_bytes(&body)?)
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 { return Err(Error::Closed); }
        filled += n;
    }
    Ok(())
}

// ── session ───────────────────────────────────────────────────────────────────

/// A session with the lockdownd service on a specific device.
pub struct LockdownSession {
    stream:      Stream,
    session_id:  Option<String>,
    device_id:   u32,
    /// Stored after start_session so connect_service can TLS-wrap service sockets.
    pair_record: Option<PairRecord>,
}

impl LockdownSession {
    pub fn connect(device_id: u32) -> Result<Self, Error> {
        let mux    = MuxConn::open()?;
        let stream = mux.open_tunnel(device_id, super::LOCKDOWN_PORT)?;
        Ok(LockdownSession {
            stream: Stream::Plain(stream),
            session_id:  None,
            device_id,
            pair_record: None,
        })
    }

    pub fn device_id(&self) -> u32 { self.device_id }

    /// Start a named service and open a fresh usbmux tunnel to its port.
    ///
    /// If the service requires SSL (`EnableServiceSSL: true`), the returned
    /// socket is already TLS-wrapped using the session's pair record.
    /// Service code treats it as a plain `MuxSocket` either way.
    pub fn connect_service(&mut self, name: &str) -> Result<MuxSocket, Error> {
        let svc    = self.start_service(name)?;
        let mux    = MuxConn::open()?;
        let socket = mux.open_tunnel(self.device_id, svc.port)?;

        if svc.enable_service_ssl {
            let pair = self.pair_record.as_ref()
                .ok_or_else(|| Error::Tls(format!(
                    "service {name} requires SSL but no pair record — call start_session first"
                )))?;
            let config = build_tls_config(pair)?;
            let server_name = ServerName::try_from("localhost")
                .map_err(|e| Error::Tls(e.to_string()))?;
            let mut conn = ClientConnection::new(Arc::new(config), server_name)
                .map_err(|e| Error::Tls(e.to_string()))?;
            let mut sock = socket;
            while conn.is_handshaking() {
                conn.complete_io(&mut sock)
                    .map_err(|e| Error::Tls(format!("service TLS handshake: {e}")))?;
            }
            // Wrap the TLS stream in MuxSocket::External so callers stay transparent
            return Ok(MuxSocket::external(StreamOwned::new(conn, sock)));
        }

        Ok(socket)
    }

    // ── session lifecycle ────────────────────────────────────────────────────

    /// Convenience: open session, read pair record from usbmuxd, call
    /// `start_session` in one step.  Returns `self` for chaining.
    pub fn open_paired(device_id: u32, udid: &str) -> Result<Self, Error> {
        let mut session     = Self::connect(device_id)?;
        let pair_record     = PairRecord::read_from_usbmuxd(udid)?;
        session.start_session(&pair_record)?;
        Ok(session)
    }

    /// Send `StartSession` and upgrade the socket to TLS if required.
    pub fn start_session(&mut self, pair: &PairRecord) -> Result<String, Error> {
        let req = StartSessionReq {
            request:     "StartSession",
            host_id:     &pair.host_id,
            system_buid: &pair.system_buid,
            label:       "devicelink",
        };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;

        let session_id = resp.session_id
            .ok_or_else(|| Error::Lockdown("StartSession: no SessionID".into()))?;
        self.session_id  = Some(session_id.clone());
        self.pair_record = Some(pair.clone());

        if resp.enable_session_ssl.unwrap_or(false) {
            self.upgrade_to_tls(pair)?;
        }
        Ok(session_id)
    }

    /// Close the active session.
    pub fn stop_session(&mut self) -> Result<(), Error> {
        let Some(sid) = self.session_id.take() else { return Ok(()); };
        let req = StopSessionReq {
            request:    "StopSession",
            session_id: &sid,
            label:      "devicelink",
        };
        send_plist(&mut self.stream, &req)?;
        // Drain and discard the acknowledgement
        let _ = recv_plist(&mut self.stream);
        Ok(())
    }

    // ── lockdown commands ────────────────────────────────────────────────────

    pub fn get_all_values(&mut self) -> Result<DeviceInfo, Error> {
        let req = Request {
            request: "GetValue",
            domain: None, key: None,
            label: Some("devicelink"),
            service: None, include_escrow_bag: None,
        };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;
        match resp.value {
            Some(Value::Dictionary(d)) => Ok(DeviceInfo::from_dict(d)),
            _ => Err(Error::Lockdown("GetValue returned unexpected payload".into())),
        }
    }

    /// Set a lockdownd value, optionally scoped to a domain.
    /// Pass `domain: None` to write to the root domain.
    pub fn set_value(&mut self, domain: Option<&str>, key: &str, value: Value) -> Result<(), Error> {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct SetValueRequest<'a> {
            request: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            domain:  Option<&'a str>,
            key:     &'a str,
            value:   plist::Value,
            label:   &'a str,
        }
        let req = SetValueRequest { request: "SetValue", domain, key, value, label: "devicelink" };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;
        if let Some(e) = resp.error {
            return Err(Error::Lockdown(format!("SetValue({key}): {e}")));
        }
        Ok(())
    }

    pub fn get_value(&mut self, domain: Option<&str>, key: &str) -> Result<Value, Error> {
        let req = Request {
            request: "GetValue",
            domain,
            key: Some(key),
            label: Some("devicelink"),
            service: None, include_escrow_bag: None,
        };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;
        resp.value.ok_or_else(|| Error::Lockdown(format!("no Value for key {key}")))
    }

    /// Returns the list of available service names.
    ///
    /// lockdownd doesn't expose a unified services catalogue; we probe multiple
    /// domains and merge what we find.  On some iOS versions the Services dict
    /// is not accessible at all, in which case an empty vec is returned.
    pub fn list_services(&mut self) -> Result<Vec<String>, Error> {
        // Some iOS versions return Services under this domain
        if let Ok(Value::Dictionary(d)) =
            self.get_value(Some("com.apple.mobile.lockdown"), "Services")
        {
            return Ok(d.keys().cloned().collect());
        }

        // Fallback: GetValue(nil, nil) and look for a Services sub-dict
        let req = Request {
            request: "GetValue",
            domain: None, key: None,
            label: Some("devicelink"),
            service: None, include_escrow_bag: None,
        };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;
        if let Some(Value::Dictionary(mut d)) = resp.value {
            if let Some(Value::Dictionary(svc)) = d.remove("Services") {
                return Ok(svc.keys().cloned().collect());
            }
        }

        // Services catalogue not available on this iOS version — not an error
        Ok(vec![])
    }

    pub fn start_service(&mut self, name: &str) -> Result<ServiceInfo, Error> {
        let req = Request {
            request: "StartService",
            domain: None, key: None,
            label: Some("devicelink"),
            service: Some(name),
            include_escrow_bag: None,
        };
        send_plist(&mut self.stream, &req)?;
        let resp = self.expect_response()?;
        match resp.port {
            Some(port) => Ok(ServiceInfo {
                port,
                enable_service_ssl: resp.enable_service_ssl.unwrap_or(false),
            }),
            None => Err(Error::Lockdown(format!(
                "StartService({name}) failed: {}",
                resp.error.as_deref().unwrap_or("no port returned")
            ))),
        }
    }

    pub fn query_type(&mut self) -> Result<String, Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct Q { request: &'static str }
        send_plist(&mut self.stream, &Q { request: "QueryType" })?;
        let val = recv_plist(&mut self.stream)?;
        match val {
            Value::Dictionary(d) => d.get("Type")
                .and_then(|v| if let Value::String(s) = v { Some(s.clone()) } else { None })
                .ok_or_else(|| Error::Lockdown("QueryType: no Type field".into())),
            _ => Err(Error::Lockdown("QueryType: unexpected response".into())),
        }
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn expect_response(&mut self) -> Result<BaseResponse, Error> {
        let val  = recv_plist(&mut self.stream)?;
        let resp: BaseResponse = plist::from_value(&val)
            .map_err(|e| Error::Lockdown(format!("response parse error: {e}")))?;
        if let Some(err) = &resp.error {
            return Err(Error::Lockdown(err.clone()));
        }
        Ok(resp)
    }

    fn upgrade_to_tls(&mut self, pair: &PairRecord) -> Result<(), Error> {
        let config = build_tls_config(pair)?;

        let old = std::mem::replace(&mut self.stream, Stream::Upgrading);
        let plain = match old {
            Stream::Plain(s) => s,
            other            => { self.stream = other; return Ok(()); } // already TLS
        };

        let server_name = ServerName::try_from("localhost")
            .map_err(|e| Error::Tls(e.to_string()))?;
        let mut conn = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| Error::Tls(e.to_string()))?;
        let mut sock = plain;

        // Complete the TLS handshake eagerly — gives a real error instead of a
        // silent EOF if the cipher suite or certificate doesn't match.
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)
                .map_err(|e| Error::Tls(format!("TLS handshake: {e}")))?;
        }

        self.stream = Stream::Tls(Box::new(StreamOwned::new(conn, sock)));
        Ok(())
    }
}

impl Drop for LockdownSession {
    fn drop(&mut self) {
        let _ = self.stop_session();
    }
}

// ── TLS config builder ────────────────────────────────────────────────────────

fn build_tls_config(pair: &PairRecord) -> Result<ClientConfig, Error> {
    use rustls_pemfile::{certs, private_key};
    use std::io::BufReader;

    // Parse host certificate chain (PEM → DER)
    let cert_chain: Vec<CertificateDer<'static>> = {
        let mut r = BufReader::new(pair.host_certificate.as_slice());
        certs(&mut r)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Error::Tls(format!("cert parse: {e}")))?
    };
    if cert_chain.is_empty() {
        return Err(Error::Tls("no certificate found in pair record HostCertificate".into()));
    }

    // Parse host private key (PEM RSA → DER)
    let private_key: PrivateKeyDer<'static> = {
        let mut r = BufReader::new(pair.host_private_key.as_slice());
        private_key(&mut r)
            .map_err(|e| Error::Tls(format!("key parse: {e}")))?
            .ok_or_else(|| Error::Tls("no private key in pair record HostPrivateKey".into()))?
    };

    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
        .with_client_auth_cert(cert_chain, private_key)
        .map_err(|e| Error::Tls(e.to_string()))?;

    Ok(config)
}
