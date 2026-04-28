/// Unified device session that routes to RSD or legacy lockdownd based on
/// iOS version and the active [`ConnectionMode`].
///
/// # Path selection
///
/// ```text
/// Auto + iOS < 17      → Legacy (usbmux → lockdownd)
/// Auto + iOS 17.0–17.3 → RSD via USB-Ethernet (TODO) → fallback Legacy
/// Auto + iOS 17.4+     → RSD via CoreDeviceProxy CDTunnel → fallback Legacy
/// Legacy               → always usbmux → lockdownd
/// Rsd                  → RSD only; error if unavailable
/// ```
///
/// # Environment / CLI override
///
/// Set `IOS_LEGACY=1` or pass `--legacy` to force the legacy path.
use std::io::{Read, Write};

use lockdown::LockdownSession;
use usbmux::Device;

use crate::error::Error;
use crate::mode::ConnectionMode;
use crate::version::{detect_version, IosVersion};

/// Active transport for a device session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePath {
    /// usbmux → lockdownd (all iOS versions)
    Legacy,
    /// iOS 17 CDTunnel → RSD (not yet operational — needs TUN/IP routing)
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
    // Rsd path: smoltcp tunnel + lockdownd session for fallback services
    Rsd { tunnel: crate::smoltcp_stack::SmoltcpTunnel, lockdown: LockdownSession },
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
    ///
    /// Reads `IOS_LEGACY` from the environment; the `mode` parameter (from
    /// `--legacy` CLI flag) is applied on top.
    pub fn open(device: Device, mode: ConnectionMode) -> Result<Self, Error> {
        let effective = ConnectionMode::from_env().with_legacy_flag(mode.is_legacy());
        let version   = detect_version(device.device_id)?;

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
                try_rsd(&device, version)
                    .map_err(|e| Error::Protocol(format!("RSD mode forced but failed: {e}")))?
            }

            ConnectionMode::Auto => {
                if version.supports_core_device_proxy() {
                    match try_rsd(&device, version) {
                        Ok((inner, path)) => (inner, path),
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

        Ok(DeviceSession {
            device,
            version,
            active_path,
            inner,
        })
    }

    /// Borrow the underlying `LockdownSession`.
    ///
    /// Available on all paths (the RSD path also proxies lockdownd services
    /// via `RSDCheckin` until native RSD service support is added).
    pub fn lockdown(&mut self) -> &mut LockdownSession {
        match &mut self.inner {
            Inner::Legacy(s)            => s,
            Inner::Rsd { lockdown, .. } => lockdown,
        }
    }

    /// Access the smoltcp tunnel (only available on the RSD path).
    pub fn smoltcp_tunnel(&mut self) -> Option<&mut crate::smoltcp_stack::SmoltcpTunnel> {
        match &mut self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            Inner::Legacy(_)          => None,
        }
    }

    /// Shared (non-mut) reference to the smoltcp tunnel.
    pub fn smoltcp_tunnel_ref(&self) -> Option<&crate::smoltcp_stack::SmoltcpTunnel> {
        match &self.inner {
            Inner::Rsd { tunnel, .. } => Some(tunnel),
            Inner::Legacy(_)          => None,
        }
    }

    /// Connect to an RSD shim service (`.shim.remote`) through the CDTunnel.
    ///
    /// Looks up the service port in the RSD catalog, connects via smoltcp,
    /// sends the `RSDCheckin` plist, reads the response, and returns the
    /// ready-to-use stream wrapped as `MuxSocket::External`.
    pub fn connect_rsd_shim(&mut self, service_name: &str) -> Result<usbmux::MuxSocket, Error> {
        // Look up port (connect_rsd briefly borrows self, then releases).
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

        // RSDCheckin handshake expected by all .shim.remote services
        let checkin = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>devicelink</string>
<key>ProtocolVersion</key><string>2</string>
<key>Request</key><string>RSDCheckin</string>
</dict></plist>
"#;
        let len = checkin.len() as u32;
        stream.write_all(&len.to_be_bytes())
            .and_then(|_| stream.write_all(checkin))
            .and_then(|_| stream.flush())
            .map_err(|e| Error::Protocol(format!("RSDCheckin write: {e}")))?;

        // Drain the shim's initial handshake messages (all 4-byte-length-prefixed plists):
        //   1. RSDCheckin acknowledgment  (Request: "RSDCheckin" or similar)
        //   2. StartService notification  (Request: "StartService")
        // Stop as soon as we see "Request: StartService" — that signals the real service
        // is ready.  Limit to 4 iterations to avoid blocking if the protocol changes.
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

        Ok(usbmux::MuxSocket::external(stream))
    }

    pub fn is_rsd(&self) -> bool { self.active_path == ActivePath::Rsd }

    /// Connect to the RSD service through the CDTunnel smoltcp stack.
    /// Only available when `active_path == Rsd`.
    pub fn connect_rsd(&mut self) -> Result<rsd::RsdClient, Error> {
        let tunnel = self.smoltcp_tunnel()
            .ok_or_else(|| Error::Protocol("RSD not available on legacy path".into()))?;
        let server_addr     = tunnel.params.server_addr;
        let server_rsd_port = tunnel.params.server_rsd_port;
        let stream = tunnel.connect(server_addr, server_rsd_port)?;
        rsd::RsdClient::connect_stream(stream)
            .map_err(|e| Error::Protocol(format!("RSD connect: {e}")))
    }
}

// ── path helpers ──────────────────────────────────────────────────────────────

fn open_legacy(device_id: u32, serial: &str) -> Result<LockdownSession, Error> {
    Ok(LockdownSession::open_paired(device_id, serial)?)
}

/// Attempt the full RSD path: RemotePairing → CDTunnel → smoltcp stack.
/// Returns `(Inner, ActivePath)` on success.
fn try_rsd(device: &Device, version: IosVersion) -> Result<(Inner, ActivePath), Error> {
    let tunnel = crate::ios17::Ios17Tunnel::connect_via_lockdown_udid(
        device.device_id, Some(&device.serial), version,
    ).map_err(|e| Error::Protocol(format!("CDTunnel/RemotePairing failed: {e}")))?;

    // Also open a lockdownd session so legacy services still work alongside RSD
    let lockdown = open_legacy(device.device_id, &device.serial)?;

    Ok((Inner::Rsd { tunnel: tunnel.stack, lockdown }, ActivePath::Rsd))
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
