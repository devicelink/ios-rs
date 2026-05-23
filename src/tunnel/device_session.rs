/// Unified device session that routes to RSD (via daemon or direct) or legacy
/// lockdownd based on iOS version and the active [`ConnectionMode`].
///
/// # Path selection
///
/// ```text
/// Auto + iOS < 17      → Legacy (usbmux → lockdownd)
/// Auto + iOS 17.4+     → Daemon path (spawn ios-rsd if needed) → fallback direct → fallback Legacy
/// Legacy               → always usbmux → lockdownd
/// Rsd                  → Daemon path → fallback direct; error if both fail
/// ```
use std::io::{Read, Write};

use crate::lockdown::LockdownSession;
use crate::usbmux::Device;

use super::error::Error;
use super::mode::ConnectionMode;
use super::version::{detect_version, IosVersion};

/// Unix socket used by the RSD tunnel daemon.
pub const DAEMON_SOCKET: &str = "/tmp/ios-rsd.sock";

/// Active transport for a device session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePath {
    /// usbmux → lockdownd (all iOS versions)
    Legacy,
    /// iOS 17 CDTunnel → RSD (direct smoltcp or via daemon)
    Rsd,
}

impl std::fmt::Display for ActivePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivePath::Legacy => write!(f, "legacy (usbmux → lockdownd)"),
            ActivePath::Rsd    => write!(f, "rsd (CDTunnel → RSD)"),
        }
    }
}

enum Inner {
    Legacy(LockdownSession),
    /// Direct smoltcp tunnel (no daemon).
    Rsd { tunnel: super::smoltcp_stack::SmoltcpTunnel, lockdown: LockdownSession },
    /// All RSD service connections are proxied through the tunnel daemon.
    Daemon { lockdown: LockdownSession, udid: String },
}

/// A session with an iOS device using the best available path.
pub struct DeviceSession {
    pub device:      Device,
    pub version:     IosVersion,
    pub active_path: ActivePath,
    inner:           Inner,
}

impl DeviceSession {
    /// Open a session for `device` using `mode` to select the path.
    pub fn open(device: Device, mode: ConnectionMode) -> Result<Self, Error> {
        let env_mode  = ConnectionMode::from_env();
        let effective = if env_mode == ConnectionMode::Legacy {
            ConnectionMode::Legacy
        } else if mode == ConnectionMode::Rsd {
            ConnectionMode::Rsd
        } else {
            env_mode.with_legacy_flag(mode.is_legacy())
        };
        let version = detect_version(device.device_id)?;

        let (inner, active_path) = match effective {
            ConnectionMode::Legacy => {
                let s = open_legacy(device.device_id, &device.serial)?;
                (Inner::Legacy(s), ActivePath::Legacy)
            }

            ConnectionMode::Rsd => {
                if !version.supports_core_device_proxy() {
                    return Err(Error::Protocol(format!(
                        "iOS {version} does not support RSD via CoreDeviceProxy (requires iOS 17.4+)"
                    )));
                }
                // Try daemon, fall back to direct smoltcp tunnel.
                try_daemon_path(&device.serial, device.device_id)
                    .or_else(|e| {
                        eprintln!("daemon unavailable ({e}), trying direct RSD…");
                        try_rsd(&device, version)
                    })
                    .map_err(|e| Error::Protocol(format!("RSD mode forced but failed: {e}")))?
            }

            ConnectionMode::Auto => {
                if version.supports_core_device_proxy() {
                    let rsd = try_daemon_path(&device.serial, device.device_id)
                        .or_else(|e| {
                            eprintln!("daemon unavailable ({e}), trying direct RSD…");
                            try_rsd(&device, version)
                        });
                    match rsd {
                        Ok(result) => result,
                        Err(e) => {
                            eprintln!(
                                "RSD path unavailable for {} (iOS {version}): {e}\n\
                                 → falling back to legacy lockdownd",
                                device.serial
                            );
                            let s = open_legacy(device.device_id, &device.serial)?;
                            (Inner::Legacy(s), ActivePath::Legacy)
                        }
                    }
                } else {
                    let s = open_legacy(device.device_id, &device.serial)?;
                    (Inner::Legacy(s), ActivePath::Legacy)
                }
            }
        };

        Ok(DeviceSession { device, version, active_path, inner })
    }

    /// Borrow the underlying `LockdownSession` (available on all paths).
    pub fn lockdown(&mut self) -> &mut LockdownSession {
        match &mut self.inner {
            Inner::Legacy(s)             => s,
            Inner::Rsd { lockdown, .. }  => lockdown,
            Inner::Daemon { lockdown, .. } => lockdown,
        }
    }

    /// Access the smoltcp tunnel (only available on the direct RSD path, not daemon).
    pub fn smoltcp_tunnel(&mut self) -> Option<&mut super::smoltcp_stack::SmoltcpTunnel> {
        match &mut self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            _                         => None,
        }
    }

    /// Shared reference to the smoltcp tunnel.
    pub fn smoltcp_tunnel_ref(&self) -> Option<&super::smoltcp_stack::SmoltcpTunnel> {
        match &self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            _                         => None,
        }
    }

    /// Connect to an RSD shim service through the best available path.
    ///
    /// Daemon path: delegates the connection (including RSDCheckin) to the daemon.
    /// Direct path: looks up port in RSD catalog, connects via smoltcp, does RSDCheckin.
    pub fn connect_rsd_shim(&mut self, service_name: &str) -> Result<crate::usbmux::MuxSocket, Error> {
        match &self.inner {
            Inner::Daemon { udid, .. } => {
                let udid = udid.clone();
                let sock = daemon_connect_service(&udid, service_name)?;
                return Ok(crate::usbmux::MuxSocket::external(sock));
            }
            _ => {}
        }

        // Direct smoltcp path — look up port in RSD catalog then connect.
        let port = {
            let rsd = self.connect_rsd()?;
            rsd.service(service_name)
                .ok_or_else(|| Error::Protocol(format!("RSD service '{service_name}' not in catalog")))?
                .port
        };

        let tunnel = self.smoltcp_tunnel_ref()
            .ok_or_else(|| Error::Protocol("no CDTunnel available".into()))?;
        let server_addr = tunnel.params.server_addr;
        let mut stream = tunnel.connect(server_addr, port)?;

        let checkin = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
<key>Label</key><string>devicelink</string>\n\
<key>ProtocolVersion</key><string>2</string>\n\
<key>Request</key><string>RSDCheckin</string>\n\
</dict></plist>\n";
        let len = checkin.len() as u32;
        stream.write_all(&len.to_be_bytes())
            .and_then(|_| stream.write_all(checkin))
            .and_then(|_| stream.flush())
            .map_err(|e| Error::Protocol(format!("RSDCheckin write: {e}")))?;

        for _ in 0..4 {
            let mut len_buf = [0u8; 4];
            read_exact(&mut stream, &mut len_buf)
                .map_err(|e| Error::Protocol(format!("RSDCheckin read len: {e}")))?;
            let resp_len = u32::from_be_bytes(len_buf) as usize;
            let mut resp_body = vec![0u8; resp_len];
            read_exact(&mut stream, &mut resp_body)
                .map_err(|e| Error::Protocol(format!("RSDCheckin read body: {e}")))?;
            if let Ok(plist::Value::Dictionary(d)) = plist::from_bytes::<plist::Value>(&resp_body) {
                let req = d.get("Request").and_then(|v| v.as_string()).unwrap_or_default();
                if req == "StartService" { break; }
            }
        }

        Ok(crate::usbmux::MuxSocket::external(stream))
    }

    /// Connect to an RSD service through the best available path, without RSDCheckin.
    ///
    /// Use this for non-shim RSD services (e.g. DTX instruments hub, xctest proxy).
    /// For `.shim.remote` services that require the RSDCheckin handshake use
    /// [`connect_rsd_shim`] instead.
    ///
    /// Daemon path: delegates port lookup and connection to the daemon.
    /// Direct path: looks up port in RSD catalog, connects via smoltcp.
    pub fn connect_rsd_service(&mut self, service_name: &str)
        -> Result<std::os::unix::net::UnixStream, Error>
    {
        match &self.inner {
            Inner::Daemon { udid, .. } => {
                let udid = udid.clone();
                return daemon_connect_service(&udid, service_name);
            }
            _ => {}
        }

        let port = {
            let rsd = self.connect_rsd()?;
            rsd.service(service_name)
                .ok_or_else(|| Error::Protocol(format!("RSD service '{service_name}' not in catalog")))?
                .port
        };

        let tunnel = self.smoltcp_tunnel_ref()
            .ok_or_else(|| Error::Protocol("no CDTunnel available".into()))?;
        let server_addr = tunnel.params.server_addr;
        tunnel.connect(server_addr, port)
    }

    pub fn is_rsd(&self) -> bool { self.active_path == ActivePath::Rsd }

    /// Connect to the RSD service catalog.
    ///
    /// Daemon path: asks the daemon to proxy the RSD port, does RemoteXPC + handshake locally.
    /// Direct path: connects smoltcp TCP to the RSD port.
    pub fn connect_rsd(&mut self) -> Result<crate::rsd::RsdClient, Error> {
        match &self.inner {
            Inner::Daemon { udid, .. } => {
                let udid = udid.clone();
                let sock = daemon_connect_service(&udid, "_rsd")?;
                return crate::rsd::RsdClient::connect_stream(sock)
                    .map_err(|e| Error::Protocol(format!("RSD via daemon: {e}")));
            }
            _ => {}
        }

        let tunnel = self.smoltcp_tunnel()
            .ok_or_else(|| Error::Protocol("RSD not available on legacy path".into()))?;
        let server_addr     = tunnel.params.server_addr;
        let server_rsd_port = tunnel.params.server_rsd_port;
        let stream = tunnel.connect(server_addr, server_rsd_port)?;
        crate::rsd::RsdClient::connect_stream(stream)
            .map_err(|e| Error::Protocol(format!("RSD connect: {e}")))
    }
}

// ── path helpers ──────────────────────────────────────────────────────────────

fn open_legacy(device_id: u32, serial: &str) -> Result<LockdownSession, Error> {
    Ok(LockdownSession::open_paired(device_id, serial)?)
}

/// Try to use the tunnel daemon (spawning it if not running).
fn try_daemon_path(udid: &str, device_id: u32) -> Result<(Inner, ActivePath), Error> {
    if !daemon_is_running() {
        spawn_daemon()?;
        wait_for_daemon()?;
    }
    let lockdown = open_legacy(device_id, udid)?;
    Ok((Inner::Daemon { lockdown, udid: udid.to_owned() }, ActivePath::Rsd))
}

fn daemon_is_running() -> bool {
    std::os::unix::net::UnixStream::connect(DAEMON_SOCKET).is_ok()
}

fn spawn_daemon() -> Result<(), Error> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::Protocol(format!("current_exe: {e}")))?;
    std::process::Command::new(&exe)
        .args(["tunnel", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Protocol(format!("spawn daemon: {e}")))?;
    Ok(())
}

fn wait_for_daemon() -> Result<(), Error> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if std::os::unix::net::UnixStream::connect(DAEMON_SOCKET).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Protocol("tunnel daemon did not start within 15 s".into()));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Direct smoltcp tunnel (no daemon): CDTunnel → smoltcp stack.
fn try_rsd(device: &Device, version: IosVersion) -> Result<(Inner, ActivePath), Error> {
    let tunnel = super::ios17::Ios17Tunnel::connect_via_lockdown_udid(
        device.device_id, Some(&device.serial), version,
    ).map_err(|e| Error::Protocol(format!("CDTunnel/RemotePairing failed: {e}")))?;

    let lockdown = open_legacy(device.device_id, &device.serial)?;
    Ok((Inner::Rsd { tunnel: tunnel.stack, lockdown }, ActivePath::Rsd))
}

// ── daemon IPC client ─────────────────────────────────────────────────────────

/// Connect to the daemon and request a proxied connection to `service` for `udid`.
/// On success returns a `UnixStream` that is a transparent byte pipe to the service.
fn daemon_connect_service(udid: &str, service: &str) -> Result<std::os::unix::net::UnixStream, Error> {
    use std::os::unix::net::UnixStream;

    let mut sock = UnixStream::connect(DAEMON_SOCKET)
        .map_err(|e| Error::Protocol(format!("daemon connect: {e}")))?;

    let mut req = plist::Dictionary::new();
    req.insert("UDID".into(),    plist::Value::String(udid.into()));
    req.insert("Service".into(), plist::Value::String(service.into()));
    let req = plist::Value::Dictionary(req);

    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &req)
        .map_err(|e| Error::Protocol(format!("plist encode: {e}")))?;
    let len = body.len() as u32;
    sock.write_all(&len.to_be_bytes())
        .and_then(|_| sock.write_all(&body))
        .and_then(|_| sock.flush())
        .map_err(|e| Error::Protocol(format!("daemon write: {e}")))?;

    let mut len_buf = [0u8; 4];
    daemon_read_exact(&mut sock, &mut len_buf)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    if n == 0 || n > 1_048_576 {
        return Err(Error::Protocol(format!("daemon: bad response length {n}")));
    }
    let mut resp_body = vec![0u8; n];
    daemon_read_exact(&mut sock, &mut resp_body)?;

    let resp: plist::Value = plist::from_bytes(&resp_body)
        .map_err(|e| Error::Protocol(format!("daemon plist: {e}")))?;
    let dict = resp.as_dictionary()
        .ok_or_else(|| Error::Protocol("daemon: response not a dict".into()))?;

    match dict.get("Status").and_then(|v| v.as_string()) {
        Some("Ok") => Ok(sock),
        Some("Error") => {
            let msg = dict.get("Error").and_then(|v| v.as_string()).unwrap_or("unknown");
            Err(Error::Protocol(format!("daemon: {msg}")))
        }
        _ => Err(Error::Protocol("daemon: unexpected status in response".into())),
    }
}

fn daemon_read_exact(s: &mut std::os::unix::net::UnixStream, buf: &mut [u8]) -> Result<(), Error> {
    let mut done = 0;
    while done < buf.len() {
        let n = s.read(&mut buf[done..])
            .map_err(|e| Error::Protocol(format!("daemon read: {e}")))?;
        if n == 0 {
            return Err(Error::Protocol("daemon: unexpected EOF".into()));
        }
        done += n;
    }
    Ok(())
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read(&mut buf[done..])?;
        if n == 0 { return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof")); }
        done += n;
    }
    Ok(())
}
