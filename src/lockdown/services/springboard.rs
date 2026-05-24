//! `com.apple.springboardservices` — SpringBoard queries (orientation, icons, wallpaper).
//!
//! Uses the same 4-byte-BE-length-prefix plist framing as installation_proxy,
//! but with lowercase command keys instead of PascalCase.
use std::io::{Read, Write};

use crate::usbmux::MuxSocket;
use plist::Value;

use super::super::{Error, LockdownSession};

const SERVICE: &str = "com.apple.springboardservices";

/// Screen orientation as reported by SpringBoard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait = 1,
    PortraitUpsideDown = 2,
    LandscapeRight = 3, // home button on the right
    LandscapeLeft = 4,  // home button on the left
}

impl Orientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::PortraitUpsideDown => "portrait_upside_down",
            Self::LandscapeRight => "landscape_right",
            Self::LandscapeLeft => "landscape_left",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "portrait" | "1" => Some(Self::Portrait),
            "portrait_upside_down" | "2" => Some(Self::PortraitUpsideDown),
            "landscape_right" | "3" => Some(Self::LandscapeRight),
            "landscape_left" | "4" => Some(Self::LandscapeLeft),
            _ => None,
        }
    }
}

impl std::fmt::Display for Orientation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pub struct SpringBoardClient {
    stream: MuxSocket,
}

impl SpringBoardClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        Ok(Self {
            stream: session.connect_service(SERVICE)?,
        })
    }

    /// Get the bundle ID of the current foreground app.
    pub fn get_foreground_app(&mut self) -> Result<String, Error> {
        let mut d = plist::Dictionary::new();
        d.insert(
            "command".into(),
            Value::String("getFrontMostDisplayIdentifier".into()),
        );
        send(&mut self.stream, &Value::Dictionary(d))?;
        let resp = recv(&mut self.stream)?;
        resp.as_dictionary()
            .and_then(|d| d.get("displayIdentifier"))
            .and_then(|v| v.as_string())
            .map(|s| s.to_owned())
            .ok_or_else(|| Error::Lockdown("no displayIdentifier in response".into()))
    }

    /// Set the interface orientation directly via SpringBoard (no XCUITest needed).
    pub fn set_orientation(&mut self, o: Orientation) -> Result<(), Error> {
        let mut d = plist::Dictionary::new();
        d.insert(
            "command".into(),
            Value::String("setInterfaceOrientation".into()),
        );
        d.insert("value".into(), Value::Integer((o as i64).into()));
        send(&mut self.stream, &Value::Dictionary(d))?;
        let _ = recv(&mut self.stream)?; // consume response
        Ok(())
    }

    /// Get the current interface orientation.
    pub fn get_orientation(&mut self) -> Result<Orientation, Error> {
        let mut d = plist::Dictionary::new();
        d.insert(
            "command".into(),
            Value::String("getInterfaceOrientation".into()),
        );
        send(&mut self.stream, &Value::Dictionary(d))?;
        let resp = recv(&mut self.stream)?;
        let n = resp
            .as_dictionary()
            .and_then(|d| d.get("interfaceOrientation"))
            .and_then(|v| v.as_signed_integer())
            .ok_or_else(|| Error::Lockdown("no interfaceOrientation in response".into()))?;
        match n {
            1 => Ok(Orientation::Portrait),
            2 => Ok(Orientation::PortraitUpsideDown),
            3 => Ok(Orientation::LandscapeRight),
            4 => Ok(Orientation::LandscapeLeft),
            _ => Err(Error::Lockdown(format!("unknown orientation value: {n}"))),
        }
    }
}

// ── framing ───────────────────────────────────────────────────────────────────

fn send(s: &mut MuxSocket, val: &Value) -> Result<(), Error> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, val)?;
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()?;
    Ok(())
}

fn recv(s: &mut MuxSocket) -> Result<Value, Error> {
    let mut len = [0u8; 4];
    read_exact(s, &mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    let mut body = vec![0u8; n];
    read_exact(s, &mut body)?;
    Ok(plist::from_bytes(&body)?)
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
