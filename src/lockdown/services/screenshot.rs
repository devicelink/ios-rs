//! Screenshot capture via `com.apple.mobile.screenshotr`.
//!
//! The service speaks the Apple Device Link (DL) protocol — a simple
//! handshake of plist-encoded arrays, each framed with a 4-byte BE
//! length prefix, followed by a single PNG request/response.
use std::io::{Read, Write};

use plist::Value;

use crate::lockdown::{Error, LockdownSession};
use crate::usbmux::MuxSocket;

const SERVICE: &str = "com.apple.mobile.screenshotr";

// ── client ────────────────────────────────────────────────────────────────────

pub struct ScreenshotClient {
    stream: MuxSocket,
}

impl ScreenshotClient {
    pub fn connect(session: &mut LockdownSession) -> Result<Self, Error> {
        let stream = session.connect_service(SERVICE)?;
        Ok(ScreenshotClient { stream })
    }

    pub fn from_stream(stream: MuxSocket) -> Self {
        ScreenshotClient { stream }
    }

    /// Capture a screenshot and return the raw PNG bytes.
    pub fn take(&mut self) -> Result<Vec<u8>, Error> {
        // 1. Receive version exchange from device
        let msg = self.recv()?;
        let arr = as_array(&msg, "DLMessageVersionExchange")?;
        // arr[2] is the versions array; pick the first (max) version
        let version = arr
            .get(2)
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| {
                v.as_unsigned_integer()
                    .or_else(|| v.as_signed_integer().map(|i| i as u64))
            })
            .unwrap_or(300);

        // 2. Acknowledge with DLVersionsOk
        self.send(&Value::Array(vec![
            Value::String("DLMessageVersionExchange".into()),
            Value::String("DLVersionsOk".into()),
            Value::Integer((version as i64).into()),
        ]))?;

        // 3. Wait for DLMessageDeviceReady
        let ready = self.recv()?;
        let ready_arr = as_array(&ready, "DLMessageDeviceReady")?;
        let msg_type = ready_arr.first().and_then(|v| v.as_string()).unwrap_or("");
        if msg_type != "DLMessageDeviceReady" {
            return Err(Error::Afc(format!(
                "screenshot: expected DLMessageDeviceReady, got {msg_type:?}"
            )));
        }

        // 4. Send screenshot request
        self.send(&Value::Array(vec![
            Value::String("DLMessageProcessMessage".into()),
            Value::Dictionary({
                let mut d = plist::Dictionary::new();
                d.insert(
                    "MessageType".into(),
                    Value::String("ScreenShotRequest".into()),
                );
                d
            }),
        ]))?;

        // 5. Receive screenshot reply
        let reply = self.recv()?;
        let reply_arr = as_array(&reply, "ScreenShotReply")?;
        let payload = reply_arr
            .get(1)
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| Error::Afc("screenshot: missing payload dict in reply".into()))?;

        let png = payload
            .get("ScreenShotData")
            .and_then(|v| {
                if let Value::Data(b) = v {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::Afc("screenshot: no ScreenShotData in reply".into()))?;

        Ok(png)
    }

    // ── framing ───────────────────────────────────────────────────────────────

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
        if len == 0 || len > 32 * 1024 * 1024 {
            return Err(Error::Afc(format!(
                "screenshot: implausible message length {len}"
            )));
        }
        let mut body = vec![0u8; len];
        read_exact(&mut self.stream, &mut body)?;
        Ok(plist::from_bytes(&body)?)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn as_array<'a>(v: &'a Value, context: &str) -> Result<&'a Vec<Value>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Afc(format!("screenshot: expected array for {context}")))
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
