use std::io::{Read, Write};

use plist::Value;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.mobile.MCInstall";

/// Setup-Assistant screen keys that can be skipped via SetCloudConfiguration.
pub const SKIP_SETUP_KEYS: &[&str] = &[
    "Accessibility",
    "AccessibilityAppearance",
    "ActionButton",
    "AgeAssurance",
    "AgeBasedSafetySettings",
    "Android",
    "Appearance",
    "AppleID",
    "AppStore",
    "Avatar",
    "Biometric",
    "CameraButton",
    "CloudStorage",
    "DeviceProtection",
    "DeviceToDeviceMigration",
    "Diagnostics",
    "Display",
    "EnableLockdownMode",
    "ExpressLanguage",
    "FileVault",
    "iCloudDiagnostics",
    "iCloudStorage",
    "iMessageAndFaceTime",
    "IntendedUser",
    "Intelligence",
    "Keyboard",
    "Language",
    "LanguageAndLocale",
    "Location",
    "LockdownMode",
    "MessagingActivationUsingPhoneNumber",
    "Multitasking",
    "OSShowCase",
    "Passcode",
    "Payment",
    "PreferredLanguage",
    "Privacy",
    "Region",
    "Registration",
    "Restore",
    "RestoreCompleted",
    "Safety",
    "SafetyAndHandling",
    "ScreenSaver",
    "ScreenTime",
    "SIMSetup",
    "Siri",
    "SoftwareUpdate",
    "SpokenLanguage",
    "TapToSetup",
    "TermsOfAddress",
    "Tips",
    "Tone",
    "TOS",
    "TouchID",
    "TrueToneDisplay",
    "TVHomeScreenSync",
    "TVProviderSignIn",
    "TVRoom",
    "UnlockWithWatch",
    "UpdateCompleted",
    "Wallpaper",
    "WatchMigration",
    "WebContentFiltering",
    "Welcome",
    "WiFi",
];

pub struct McInstallClient {
    stream: MuxSocket,
}

impl McInstallClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(Self {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Must be called first before any other operation.
    pub fn flush(&mut self) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert("RequestType".into(), Value::String("Flush".into()));
        self.send_recv_ack(&Value::Dictionary(req), "Flush")
    }

    /// Query the current cloud/supervision configuration.
    pub fn get_cloud_config(&mut self) -> Result<plist::Dictionary, Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "RequestType".into(),
            Value::String("GetCloudConfiguration".into()),
        );
        self.send(&Value::Dictionary(req))?;
        self.recv()?.as_dictionary().cloned().ok_or_else(|| {
            Error::Lockdown("MCInstall: GetCloudConfiguration response not a dict".into())
        })
    }

    /// Perform the host-identifier handshake.
    pub fn hello(&mut self) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "RequestType".into(),
            Value::String("HelloHostIdentifier".into()),
        );
        // Response may or may not have Status; we don't fail on it
        self.send(&Value::Dictionary(req))?;
        let _ = self.recv()?;
        Ok(())
    }

    /// Push a cloud configuration (skips setup, sets supervision, etc.).
    pub fn set_cloud_config(&mut self, config: plist::Dictionary) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "RequestType".into(),
            Value::String("SetCloudConfiguration".into()),
        );
        req.insert("CloudConfiguration".into(), Value::Dictionary(config));
        self.send_recv_ack(&Value::Dictionary(req), "SetCloudConfiguration")
    }

    /// Step 1 of supervised escalation.  Returns the challenge bytes the device sends.
    pub fn escalate(&mut self, supervisor_cert_der: &[u8]) -> Result<Vec<u8>, Error> {
        let mut req = plist::Dictionary::new();
        req.insert("RequestType".into(), Value::String("Escalate".into()));
        req.insert(
            "SupervisorCertificate".into(),
            Value::Data(supervisor_cert_der.to_vec()),
        );
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        let dict = resp
            .as_dictionary()
            .ok_or_else(|| Error::Lockdown("MCInstall Escalate: response not a dict".into()))?;
        check_status(dict, "Escalate")?;
        match dict.get("Challenge") {
            Some(Value::Data(b)) => Ok(b.clone()),
            _ => Err(Error::Lockdown(
                "MCInstall Escalate: no Challenge in response".into(),
            )),
        }
    }

    /// Step 2 of supervised escalation: send signed challenge.
    pub fn escalate_response(&mut self, signed: &[u8]) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "RequestType".into(),
            Value::String("EscalateResponse".into()),
        );
        req.insert("SignedRequest".into(), Value::Data(signed.to_vec()));
        self.send_recv_ack(&Value::Dictionary(req), "EscalateResponse")
    }

    /// Step 3 of supervised escalation: finalise keybag migration.
    pub fn proceed_keybag_migration(&mut self) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "RequestType".into(),
            Value::String("ProceedWithKeybagMigration".into()),
        );
        self.send_recv_ack(&Value::Dictionary(req), "ProceedWithKeybagMigration")
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn send_recv_ack(&mut self, req: &Value, op: &str) -> Result<(), Error> {
        self.send(req)?;
        let resp = self.recv()?;
        let dict = resp
            .as_dictionary()
            .ok_or_else(|| Error::Lockdown(format!("MCInstall {op}: response not a dict")))?;
        check_status(dict, op)
    }

    fn send(&mut self, value: &Value) -> Result<(), Error> {
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, value)?;
        let len = body.len() as u32;
        self.stream.write_all(&len.to_be_bytes())?;
        self.stream.write_all(&body)?;
        self.stream.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Value, Error> {
        let mut len_buf = [0u8; 4];
        read_exact(&mut self.stream, &mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 8 * 1024 * 1024 {
            return Err(Error::Lockdown(format!(
                "MCInstall: bad response length {len}"
            )));
        }
        let mut body = vec![0u8; len];
        read_exact(&mut self.stream, &mut body)?;
        Ok(plist::from_bytes(&body)?)
    }
}

fn check_status(dict: &plist::Dictionary, op: &str) -> Result<(), Error> {
    match dict.get("Status").and_then(|v| v.as_string()) {
        Some("Acknowledged") | None => Ok(()),
        Some(s) => {
            let detail = dict
                .get("Error")
                .and_then(|v| v.as_string())
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            Err(Error::Lockdown(format!(
                "MCInstall {op}: status {s:?}{detail}"
            )))
        }
    }
}

fn read_exact(s: &mut MuxSocket, buf: &mut [u8]) -> Result<(), Error> {
    let mut done = 0;
    while done < buf.len() {
        let n = s.read(&mut buf[done..])?;
        if n == 0 {
            return Err(Error::Closed);
        }
        done += n;
    }
    Ok(())
}
