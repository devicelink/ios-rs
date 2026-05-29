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
use std::io::Read;
#[cfg(unix)]
use std::io::Write;

use crate::lockdown::LockdownSession;
use crate::usbmux::Device;

use super::error::Error;
use super::mode::ConnectionMode;
use super::version::{detect_version, IosVersion};

/// Default Unix socket path for the RSD tunnel daemon.
pub const DAEMON_SOCKET: &str = "/tmp/ios-rsd.sock";

/// Environment variable that overrides where the daemon listens / where clients connect.
///
/// Accepted formats (mirrors `USBMUXD_SOCKET_ADDRESS` convention):
///   `unix:///tmp/ios-rsd.sock`  — Unix domain socket (default)
///   `tcp://127.0.0.1:7776`      — TCP socket
pub const DAEMON_SOCKET_ENV: &str = "IOS_TUNNEL_SOCKET_ADDRESS";

/// Active transport for a device session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePath {
    /// usbmux → lockdownd (all iOS versions)
    Legacy,
    /// iOS 17 CDTunnel → RSD (direct smoltcp or via daemon)
    #[cfg(unix)]
    Rsd,
}

impl std::fmt::Display for ActivePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivePath::Legacy => write!(f, "legacy (usbmux → lockdownd)"),
            #[cfg(unix)]
            ActivePath::Rsd => write!(f, "rsd (CDTunnel → RSD)"),
        }
    }
}

enum Inner {
    Legacy(LockdownSession),
    /// Direct smoltcp tunnel (no daemon).
    #[cfg(unix)]
    Rsd {
        tunnel: super::smoltcp_stack::SmoltcpTunnel,
        lockdown: LockdownSession,
    },
    /// All RSD service connections are proxied through the tunnel daemon.
    #[cfg(unix)]
    Daemon {
        lockdown: LockdownSession,
        udid: String,
    },
}

/// A session with an iOS device using the best available path.
pub struct DeviceSession {
    pub device: Device,
    pub version: IosVersion,
    pub active_path: ActivePath,
    inner: Inner,
}

impl DeviceSession {
    /// Open a session for `device` using `mode` to select the path.
    pub fn open(device: Device, mode: ConnectionMode) -> Result<Self, Error> {
        let env_mode = ConnectionMode::from_env();
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
                #[cfg(unix)]
                {
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
                #[cfg(not(unix))]
                {
                    return Err(Error::Protocol("RSD not supported on Windows".into()));
                }
            }

            ConnectionMode::Auto => {
                #[cfg(unix)]
                {
                    if version.supports_core_device_proxy() {
                        let rsd = try_daemon_path(&device.serial, device.device_id).or_else(|e| {
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
                #[cfg(not(unix))]
                {
                    let s = open_legacy(device.device_id, &device.serial)?;
                    (Inner::Legacy(s), ActivePath::Legacy)
                }
            }
        };

        Ok(DeviceSession {
            device,
            version,
            active_path,
            inner,
        })
    }

    /// Borrow the underlying `LockdownSession` (available on all paths).
    pub fn lockdown(&mut self) -> &mut LockdownSession {
        match &mut self.inner {
            Inner::Legacy(s) => s,
            #[cfg(unix)]
            Inner::Rsd { lockdown, .. } => lockdown,
            #[cfg(unix)]
            Inner::Daemon { lockdown, .. } => lockdown,
        }
    }

    /// Access the smoltcp tunnel (only available on the direct RSD path, not daemon).
    #[cfg(unix)]
    pub fn smoltcp_tunnel(&mut self) -> Option<&mut super::smoltcp_stack::SmoltcpTunnel> {
        match &mut self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            _ => None,
        }
    }

    /// Shared reference to the smoltcp tunnel.
    #[cfg(unix)]
    pub fn smoltcp_tunnel_ref(&self) -> Option<&super::smoltcp_stack::SmoltcpTunnel> {
        match &self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            _ => None,
        }
    }

    /// Connect to an RSD shim service through the best available path.
    ///
    /// Daemon path: delegates the connection (including RSDCheckin) to the daemon.
    /// Direct path: looks up port in RSD catalog, connects via smoltcp, does RSDCheckin.
    #[cfg(unix)]
    pub fn connect_rsd_shim(
        &mut self,
        service_name: &str,
    ) -> Result<crate::usbmux::MuxSocket, Error> {
        if let Inner::Daemon { udid, .. } = &self.inner {
            let udid = udid.clone();
            let sock = daemon_connect_service(&udid, service_name)?;
            return Ok(crate::usbmux::MuxSocket::external(sock));
        }

        // Direct smoltcp path — look up port in RSD catalog then connect.
        let port = {
            let rsd = self.connect_rsd()?;
            rsd.service(service_name)
                .ok_or_else(|| {
                    Error::Protocol(format!("RSD service '{service_name}' not in catalog"))
                })?
                .port
        };

        let tunnel = self
            .smoltcp_tunnel_ref()
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
        stream
            .write_all(&len.to_be_bytes())
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
                let req = d
                    .get("Request")
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                if req == "StartService" {
                    break;
                }
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
    #[cfg(unix)]
    pub fn connect_rsd_service(
        &mut self,
        service_name: &str,
    ) -> Result<crate::usbmux::MuxSocket, Error> {
        if let Inner::Daemon { udid, .. } = &self.inner {
            let udid = udid.clone();
            return daemon_connect_service(&udid, service_name);
        }

        let port = {
            let rsd = self.connect_rsd()?;
            rsd.service(service_name)
                .ok_or_else(|| {
                    Error::Protocol(format!("RSD service '{service_name}' not in catalog"))
                })?
                .port
        };

        let tunnel = self
            .smoltcp_tunnel_ref()
            .ok_or_else(|| Error::Protocol("no CDTunnel available".into()))?;
        let server_addr = tunnel.params.server_addr;
        tunnel
            .connect(server_addr, port)
            .map(crate::usbmux::MuxSocket::Unix)
    }

    pub fn is_rsd(&self) -> bool {
        #[cfg(unix)]
        {
            self.active_path == ActivePath::Rsd
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Connect to the RSD service catalog.
    ///
    /// Daemon path: asks the daemon to proxy the RSD port, does RemoteXPC + handshake locally.
    /// Direct path: connects smoltcp TCP to the RSD port.
    #[cfg(unix)]
    pub fn connect_rsd(&mut self) -> Result<crate::rsd::RsdClient, Error> {
        if let Inner::Daemon { udid, .. } = &self.inner {
            let udid = udid.clone();
            let sock = daemon_connect_service(&udid, "_rsd")?;
            return crate::rsd::RsdClient::connect_mux_stream(sock)
                .map_err(|e| Error::Protocol(format!("RSD via daemon: {e}")));
        }

        let tunnel = self
            .smoltcp_tunnel()
            .ok_or_else(|| Error::Protocol("RSD not available on legacy path".into()))?;
        let server_addr = tunnel.params.server_addr;
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
#[cfg(unix)]
fn try_daemon_path(udid: &str, device_id: u32) -> Result<(Inner, ActivePath), Error> {
    if !daemon_is_running() {
        spawn_daemon()?;
        wait_for_daemon()?;
    }
    let lockdown = open_legacy(device_id, udid)?;
    Ok((
        Inner::Daemon {
            lockdown,
            udid: udid.to_owned(),
        },
        ActivePath::Rsd,
    ))
}

#[cfg(unix)]
fn daemon_is_running() -> bool {
    open_daemon_conn().is_ok()
}

#[cfg(unix)]
fn spawn_daemon() -> Result<(), Error> {
    let exe = std::env::current_exe().map_err(|e| Error::Protocol(format!("current_exe: {e}")))?;
    std::process::Command::new(&exe)
        .args(["tunnel", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Protocol(format!("spawn daemon: {e}")))?;
    Ok(())
}

#[cfg(unix)]
fn wait_for_daemon() -> Result<(), Error> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if open_daemon_conn().is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Protocol(
                "tunnel daemon did not start within 15 s".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Direct smoltcp tunnel (no daemon): CDTunnel → smoltcp stack.
#[cfg(unix)]
fn try_rsd(device: &Device, version: IosVersion) -> Result<(Inner, ActivePath), Error> {
    let tunnel = super::ios17::Ios17Tunnel::connect_via_lockdown_udid(
        device.device_id,
        Some(&device.serial),
        version,
    )
    .map_err(|e| Error::Protocol(format!("CDTunnel/RemotePairing failed: {e}")))?;

    let lockdown = open_legacy(device.device_id, &device.serial)?;
    Ok((
        Inner::Rsd {
            tunnel: tunnel.stack,
            lockdown,
        },
        ActivePath::Rsd,
    ))
}

// ── daemon IPC client ─────────────────────────────────────────────────────────

/// Open a raw connection to the daemon, honouring `IOS_TUNNEL_SOCKET_ADDRESS`.
#[cfg(unix)]
fn open_daemon_conn() -> Result<crate::usbmux::MuxSocket, Error> {
    let addr =
        std::env::var(DAEMON_SOCKET_ENV).unwrap_or_else(|_| format!("unix://{DAEMON_SOCKET}"));
    if let Some(path) = addr.strip_prefix("unix://") {
        std::os::unix::net::UnixStream::connect(path)
            .map(crate::usbmux::MuxSocket::Unix)
            .map_err(|e| Error::Protocol(format!("daemon connect {path}: {e}")))
    } else if let Some(hostport) = addr.strip_prefix("tcp://") {
        std::net::TcpStream::connect(hostport)
            .map(crate::usbmux::MuxSocket::Tcp)
            .map_err(|e| Error::Protocol(format!("daemon connect {hostport}: {e}")))
    } else {
        Err(Error::Protocol(format!(
            "{DAEMON_SOCKET_ENV} must start with unix:// or tcp://: {addr}"
        )))
    }
}

/// Connect to the daemon and request a proxied connection to `service` for `udid`.
/// On success the returned `MuxSocket` is a transparent byte pipe to the service.
#[cfg(unix)]
fn daemon_connect_service(udid: &str, service: &str) -> Result<crate::usbmux::MuxSocket, Error> {
    let mut sock = open_daemon_conn()?;

    let mut req = plist::Dictionary::new();
    req.insert("UDID".into(), plist::Value::String(udid.into()));
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

    let resp: plist::Value =
        plist::from_bytes(&resp_body).map_err(|e| Error::Protocol(format!("daemon plist: {e}")))?;
    let dict = resp
        .as_dictionary()
        .ok_or_else(|| Error::Protocol("daemon: response not a dict".into()))?;

    match dict.get("Status").and_then(|v| v.as_string()) {
        Some("Ok") => Ok(sock),
        Some("Error") => {
            let msg = dict
                .get("Error")
                .and_then(|v| v.as_string())
                .unwrap_or("unknown");
            Err(Error::Protocol(format!("daemon: {msg}")))
        }
        _ => Err(Error::Protocol(
            "daemon: unexpected status in response".into(),
        )),
    }
}

fn daemon_read_exact(s: &mut impl Read, buf: &mut [u8]) -> Result<(), Error> {
    let mut done = 0;
    while done < buf.len() {
        let n = s
            .read(&mut buf[done..])
            .map_err(|e| Error::Protocol(format!("daemon read: {e}")))?;
        if n == 0 {
            return Err(Error::Protocol("daemon: unexpected EOF".into()));
        }
        done += n;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read(&mut buf[done..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof",
            ));
        }
        done += n;
    }
    Ok(())
}
