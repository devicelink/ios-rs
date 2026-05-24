/// Wire framing for RemotePairing messages over a CoreDeviceProxy service connection.
///
/// `CoreDeviceTunnelProxy` goes through a lockdownd `ServiceConnection` which
/// uses the same 4-byte BE length-prefix plist framing as lockdownd.
/// Messages are raw plist dicts — no `ControlChannelMessageEnvelope` wrapper
/// (that wrapper is only used by the XPC/RemoteXPC path).
use std::io::{Read, Write};

use crate::tunnel::error::Error;

pub fn send(stream: &mut dyn Write, msg: &plist::Value) -> Result<(), Error> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, msg)
        .map_err(|e| Error::Protocol(format!("plist encode: {e}")))?;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

pub fn recv(stream: &mut dyn Read) -> Result<plist::Value, Error> {
    let mut len_buf = [0u8; 4];
    read_exact(stream, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    read_exact(stream, &mut body)?;
    plist::from_bytes(&body).map_err(|e| Error::Protocol(format!("RemotePairing plist recv: {e}")))
}

fn read_exact(s: &mut dyn Read, buf: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = s.read(&mut buf[filled..])?;
        if n == 0 {
            return Err(Error::Protocol("RemotePairing: connection closed".into()));
        }
        filled += n;
    }
    Ok(())
}
