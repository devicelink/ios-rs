use std::io::{Read, Write};

use plist::Value;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.mobile.wireless_lockdown";

pub struct WirelessClient {
    stream: MuxSocket,
}

impl WirelessClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(WirelessClient {
            stream: session.connect_service(SERVICE)?,
        })
    }

    pub fn get_wifi_enabled(&mut self) -> Result<bool, Error> {
        let mut req = plist::Dictionary::new();
        req.insert("Request".into(), Value::String("GetWifiConnections".into()));
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        check_error(&resp, "GetWifiConnections")?;
        Ok(resp
            .as_dictionary()
            .and_then(|d| d.get("EnableWifi"))
            .and_then(|v| v.as_boolean())
            .unwrap_or(false))
    }

    pub fn set_wifi_enabled(&mut self, enabled: bool) -> Result<(), Error> {
        let mut req = plist::Dictionary::new();
        req.insert(
            "Request".into(),
            Value::String("EnableWifiConnections".into()),
        );
        req.insert("EnableWifi".into(), Value::Boolean(enabled));
        self.send(&Value::Dictionary(req))?;
        let resp = self.recv()?;
        check_error(&resp, "EnableWifiConnections")?;
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
        if len == 0 || len > 1024 * 1024 {
            return Err(Error::Lockdown(format!(
                "wireless: bad response length {len}"
            )));
        }
        let mut body = vec![0u8; len];
        read_exact(&mut self.stream, &mut body)?;
        Ok(plist::from_bytes(&body)?)
    }
}

fn check_error(resp: &Value, op: &str) -> Result<(), Error> {
    if let Some(err) = resp
        .as_dictionary()
        .and_then(|d| d.get("Error"))
        .and_then(|v| v.as_string())
    {
        return Err(Error::Lockdown(format!("wireless {op}: {err}")));
    }
    Ok(())
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
