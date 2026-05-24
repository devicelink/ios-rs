use std::io::{Read, Write};

use plist::Value;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.amfi.lockdown";

pub struct AmfiClient {
    stream: MuxSocket,
}

impl AmfiClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(AmfiClient {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Returns `true` if developer mode is currently enabled.
    pub fn developer_mode_status(&mut self) -> Result<bool, Error> {
        let mut req = plist::Dictionary::new();
        req.insert("EnableDeveloperMode".into(), Value::Boolean(false));
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        if let Some(err) = resp
            .as_dictionary()
            .and_then(|d| d.get("Error"))
            .and_then(|v| v.as_string())
        {
            return Err(Error::Lockdown(format!("amfi status: {err}")));
        }
        Ok(resp
            .as_dictionary()
            .and_then(|d| d.get("DeveloperModeEnabled"))
            .and_then(|v| v.as_boolean())
            .unwrap_or(false))
    }

    /// Enable developer mode.  Returns `true` if a reboot is required.
    pub fn enable_developer_mode(&mut self) -> Result<bool, Error> {
        let mut req = plist::Dictionary::new();
        req.insert("EnableDeveloperMode".into(), Value::Boolean(true));
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        if let Some(err) = resp
            .as_dictionary()
            .and_then(|d| d.get("Error"))
            .and_then(|v| v.as_string())
        {
            return Err(Error::Lockdown(format!("amfi enable: {err}")));
        }
        let reboot_required = resp
            .as_dictionary()
            .and_then(|d| d.get("RebootRequired"))
            .and_then(|v| v.as_boolean())
            .unwrap_or(false);
        Ok(reboot_required)
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
        if len == 0 || len > 1024 * 1024 {
            return Err(Error::Lockdown(format!("amfi: bad response length {len}")));
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
