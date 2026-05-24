use std::io::{Read, Write};

use plist::Value;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.mobileactivationd";

pub struct ActivationClient {
    stream: MuxSocket,
}

impl ActivationClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(Self {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Get tunnel session info from the device (first DRM step).
    /// Returns the raw `Value` dictionary that must be posted to Apple's drmHandshake.
    pub fn create_tunnel1_session_info(&mut self) -> Result<plist::Dictionary, Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "Command".into(),
            Value::String("CreateTunnel1SessionInfoRequest".into()),
        );
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        resp.as_dictionary()
            .and_then(|d| d.get("Value"))
            .and_then(|v| v.as_dictionary())
            .cloned()
            .ok_or_else(|| Error::Lockdown("activation: no Value in session info response".into()))
    }

    /// Use Apple's DRM response to create the activation info (second DRM step).
    /// Returns the activation info dict to post to Apple's deviceActivation.
    pub fn create_activation_info(&mut self, drm_bytes: &[u8]) -> Result<plist::Dictionary, Error> {
        let mut opts = plist::Dictionary::new();
        opts.insert(
            "BasebandWaitCount".into(),
            Value::Integer(plist::Integer::from(90_u64)),
        );

        let mut req = plist::Dictionary::new();
        req.insert(
            "Command".into(),
            Value::String("CreateActivationInfoRequest".into()),
        );
        req.insert("Value".into(), Value::Data(drm_bytes.to_vec()));
        req.insert("Options".into(), Value::Dictionary(opts));
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        resp.as_dictionary()
            .and_then(|d| d.get("Value"))
            .and_then(|v| v.as_dictionary())
            .cloned()
            .ok_or_else(|| {
                Error::Lockdown("activation: no Value in activation info response".into())
            })
    }

    /// Install Apple's activation response on the device.
    pub fn handle_activation_with_session(
        &mut self,
        activation_response: &[u8],
        headers: plist::Dictionary,
    ) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "Command".into(),
            Value::String("HandleActivationInfoWithSessionRequest".into()),
        );
        req.insert("Value".into(), Value::Data(activation_response.to_vec()));
        req.insert(
            "ActivationResponseHeaders".into(),
            Value::Dictionary(headers),
        );
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        if let Some(err) = resp
            .as_dictionary()
            .and_then(|d| d.get("Error"))
            .and_then(|v| v.as_string())
        {
            return Err(Error::Lockdown(format!("handle activation: {err}")));
        }
        Ok(())
    }

    /// Deactivate the device.
    pub fn deactivate(&mut self) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert("Request".into(), Value::String("Deactivate".into()));
        self.send(&Value::Dictionary(req))?;
        let _resp = self.recv()?;
        Ok(())
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
                "activation: bad response length {len}"
            )));
        }
        let mut body = vec![0u8; len];
        read_exact(&mut self.stream, &mut body)?;
        Ok(plist::from_bytes(&body)?)
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
