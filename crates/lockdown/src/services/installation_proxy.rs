//! `com.apple.mobile.installation_proxy` — list, install, and uninstall apps.
use std::io::{Read, Write};

use plist::Value;
use usbmux::MuxSocket;

use crate::{Error, LockdownSession};

const SERVICE: &str = "com.apple.mobile.installation_proxy";

// ── public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub enum AppType {
    #[default]
    User,
    System,
    Any,
}

impl AppType {
    fn as_str(self) -> &'static str {
        match self {
            AppType::User   => "User",
            AppType::System => "System",
            AppType::Any    => "Any",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub bundle_id:     String,
    pub name:          String,
    pub version:       String,
    pub short_version: String,
    pub app_type:      String,
    pub path:          String,
}

// ── client ────────────────────────────────────────────────────────────────────

pub struct InstallationProxy {
    stream: MuxSocket,
}

impl InstallationProxy {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(InstallationProxy { stream: session.connect_service(SERVICE)? })
    }

    /// Build an `InstallationProxy` from an already-open stream (e.g. via RSD shim).
    /// The caller is responsible for any checkin protocol before calling this.
    pub fn from_stream(stream: MuxSocket) -> Self {
        InstallationProxy { stream }
    }

    // ── commands ─────────────────────────────────────────────────────────────

    /// List installed apps.
    pub fn list_apps(&mut self, app_type: AppType) -> Result<Vec<AppInfo>, Error> {
        let req = plist_dict([
            ("Command", Value::String("Browse".into())),
            ("ClientOptions", Value::Dictionary({
                let mut d = plist::Dictionary::new();
                d.insert("ApplicationType".into(),
                    Value::String(app_type.as_str().into()));
                d.insert("ReturnAttributes".into(), Value::Array(vec![
                    Value::String("CFBundleIdentifier".into()),
                    Value::String("CFBundleDisplayName".into()),
                    Value::String("CFBundleName".into()),
                    Value::String("CFBundleShortVersionString".into()),
                    Value::String("CFBundleVersion".into()),
                    Value::String("ApplicationType".into()),
                    Value::String("Path".into()),
                ]));
                d
            })),
        ]);
        send(&mut self.stream, &req)?;

        // installation_proxy streams partial results until Status == "Complete"
        let mut apps = Vec::new();
        loop {
            let resp = recv(&mut self.stream)?;
            let dict = match &resp {
                Value::Dictionary(d) => d,
                _ => return Err(Error::Lockdown("unexpected browse response".into())),
            };

            // Accumulate any partial list included in this packet
            if let Some(Value::Array(list)) = dict.get("CurrentList") {
                for entry in list {
                    if let Some(info) = parse_app_info(entry) {
                        apps.push(info);
                    }
                }
            }

            match dict.get("Status").and_then(|v| v.as_string()) {
                Some("Complete") => break,
                Some("BrowsingApplications") => continue,
                Some(s) => return Err(Error::Lockdown(format!("browse: unexpected status {s}"))),
                None => {
                    if let Some(e) = dict.get("Error").and_then(|v| v.as_string()) {
                        return Err(Error::Lockdown(e.to_string()));
                    }
                    break;
                }
            }
        }
        Ok(apps)
    }

    /// Uninstall an app by bundle ID.
    pub fn uninstall(&mut self, bundle_id: &str) -> Result<(), Error> {
        let req = plist_dict([
            ("Command", Value::String("Uninstall".into())),
            ("ApplicationIdentifier", Value::String(bundle_id.into())),
        ]);
        send(&mut self.stream, &req)?;
        self.await_complete("uninstall")
    }

    /// Install an IPA that has already been staged on the device.
    ///
    /// `staged_path` is the device-side path, e.g. `/PublicStaging/myapp.ipa`.
    /// Use [`crate::services::afc::AfcClient`] to transfer the IPA first.
    pub fn install_staged(&mut self, staged_path: &str) -> Result<(), Error> {
        let req = plist_dict([
            ("Command", Value::String("Install".into())),
            ("PackagePath", Value::String(staged_path.into())),
        ]);
        send(&mut self.stream, &req)?;
        self.await_complete("install")
    }

    // ── internals ─────────────────────────────────────────────────────────────

    fn await_complete(&mut self, op: &str) -> Result<(), Error> {
        loop {
            let resp = recv(&mut self.stream)?;
            let dict = match &resp {
                Value::Dictionary(d) => d,
                _ => return Err(Error::Lockdown(format!("{op}: unexpected response"))),
            };

            if let Some(e) = dict.get("Error").and_then(|v| v.as_string()) {
                let desc = dict.get("ErrorDescription")
                    .and_then(|v| v.as_string())
                    .unwrap_or("");
                return Err(Error::Lockdown(format!("{op} error: {e} — {desc}")));
            }

            match dict.get("Status").and_then(|v| v.as_string()) {
                Some("Complete") => return Ok(()),
                None => return Err(Error::Lockdown(format!("{op}: no Status in response"))),
                Some(_) => continue, // progress update
            }
        }
    }
}

// ── framing (u32-BE length prefix + plist) ───────────────────────────────────

fn send(s: &mut MuxSocket, val: &Value) -> Result<(), Error> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, val)?;
    let len = body.len() as u32;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()?;
    Ok(())
}

fn recv(s: &mut MuxSocket) -> Result<Value, Error> {
    let mut len_buf = [0u8; 4];
    read_exact(s, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    read_exact(s, &mut body)?;
    Ok(plist::from_bytes(&body)?)
}

fn read_exact(s: &mut MuxSocket, buf: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = s.read(&mut buf[filled..])?;
        if n == 0 { return Err(Error::Closed); }
        filled += n;
    }
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn plist_dict<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut d = plist::Dictionary::new();
    for (k, v) in pairs { d.insert(k.into(), v); }
    Value::Dictionary(d)
}

fn parse_app_info(v: &Value) -> Option<AppInfo> {
    let d = v.as_dictionary()?;
    let s = |key: &str| -> String {
        d.get(key).and_then(|v| v.as_string()).unwrap_or_default().to_string()
    };
    let name = {
        let display = s("CFBundleDisplayName");
        if display.is_empty() { s("CFBundleName") } else { display }
    };
    Some(AppInfo {
        bundle_id:     s("CFBundleIdentifier"),
        name,
        version:       s("CFBundleVersion"),
        short_version: s("CFBundleShortVersionString"),
        app_type:      s("ApplicationType"),
        path:          s("Path"),
    })
}
