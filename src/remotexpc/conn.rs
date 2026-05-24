/// RemoteXPC connection over a TCP socket.
///
/// Protocol layers:
///   TCP → HTTP/2 framing → XPC binary messages
///
/// Two logical H2 streams:
///   Stream 1 (CS, ClientServer): host sends requests, device replies
///   Stream 3 (SC, ServerClient): device sends unsolicited events
use std::io::{BufReader, BufWriter, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;

use crate::xpc::{decode_message, encode_message, flags, Message, Value};

use super::error::Error;
use super::h2::{self, Frame};

const STREAM_CS: u32 = 1;
const STREAM_SC: u32 = 3;

pub struct RemoteXpcConn {
    reader: Mutex<BufReader<TcpStream>>,
    writer: Mutex<BufWriter<TcpStream>>,
    recv_buf: Mutex<Vec<u8>>,
    next_msg_id: Mutex<u64>,
}

impl RemoteXpcConn {
    /// Connect and complete the RemoteXPC + HTTP/2 handshake.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, Error> {
        let tcp = TcpStream::connect(addr)?;
        let conn = RemoteXpcConn {
            reader: Mutex::new(BufReader::new(tcp.try_clone()?)),
            writer: Mutex::new(BufWriter::new(tcp)),
            recv_buf: Mutex::new(Vec::new()),
            next_msg_id: Mutex::new(1),
        };
        conn.handshake()?;
        Ok(conn)
    }

    // ── public API ───────────────────────────────────────────────────────────

    /// Send an XPC value as a request on the CS stream. Returns the message id.
    pub fn send(&self, value: Value) -> Result<u64, Error> {
        let msg_id = self.alloc_msg_id();
        let msg = Message {
            flags: flags::ALWAYS_SET | flags::DATA_PRESENT | flags::WANTING_REPLY,
            msg_id,
            body: Some(value),
        };
        self.write_xpc_data(STREAM_CS, &msg)?;
        Ok(msg_id)
    }

    /// Block until the next complete XPC message arrives.
    ///
    /// Accumulates H2 DATA frames until the XPC `body_len` field is satisfied,
    /// handling the common case where the iOS device splits large messages (e.g.
    /// the RSD Handshake at ~17 KiB) across two HTTP/2 frames.
    pub fn receive(&self) -> Result<Message, Error> {
        let mut recv_buf = self.recv_buf.lock().unwrap();
        let mut r = self.reader.lock().unwrap();

        loop {
            // If recv_buf already contains a complete XPC message, decode it.
            if recv_buf.len() >= 24 {
                let body_len = u64::from_le_bytes(recv_buf[8..16].try_into().unwrap()) as usize;
                let total = 24 + body_len;
                if recv_buf.len() >= total {
                    let (msg, _) = decode_message(&recv_buf[..total])?;
                    recv_buf.drain(..total);
                    return Ok(msg);
                }
            }

            // Need more bytes — read the next H2 frame.
            let frame = h2::read_frame(&mut *r)?;
            match frame {
                Frame::Data { payload, .. } if !payload.is_empty() => {
                    recv_buf.extend_from_slice(&payload);
                }
                Frame::Settings { flags: f, .. } if f & h2::FLAG_ACK == 0 => {
                    drop(r);
                    let mut w = self.writer.lock().unwrap();
                    h2::write_settings_ack(&mut *w)?;
                    w.flush()?;
                    drop(w);
                    r = self.reader.lock().unwrap();
                }
                Frame::Ping { ack: false, opaque } => {
                    drop(r);
                    let mut w = self.writer.lock().unwrap();
                    h2::write_ping_ack(&mut *w, opaque)?;
                    w.flush()?;
                    drop(w);
                    r = self.reader.lock().unwrap();
                }
                Frame::GoAway { .. } => return Err(Error::Closed),
                _ => {} // HEADERS, WINDOW_UPDATE, etc.
            }
        }
    }

    /// Send a request and wait for the matching reply.
    pub fn request(&self, value: Value) -> Result<Value, Error> {
        let msg_id = self.send(value)?;
        loop {
            let msg = self.receive()?;
            if msg.msg_id == msg_id || msg.flags & flags::REPLY != 0 {
                return msg.body.ok_or(Error::Closed);
            }
        }
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn alloc_msg_id(&self) -> u64 {
        let mut id = self.next_msg_id.lock().unwrap();
        let v = *id;
        *id += 2; // client uses odd IDs; server uses even
        v
    }

    fn write_xpc_data(&self, stream_id: u32, msg: &Message) -> Result<(), Error> {
        let bytes = encode_message(msg);
        let mut w = self.writer.lock().unwrap();
        h2::write_data(&mut *w, stream_id, &bytes)?;
        w.flush()?;
        Ok(())
    }

    fn handshake(&self) -> Result<(), Error> {
        {
            let mut w = self.writer.lock().unwrap();

            // 1. HTTP/2 client preface
            w.write_all(h2::CLIENT_PREFACE)?;

            // 2. SETTINGS
            h2::write_settings(
                &mut *w,
                &[
                    (h2::SETTING_MAX_CONCURRENT_STREAMS, 100),
                    (h2::SETTING_INITIAL_WINDOW_SIZE, 1_048_576),
                ],
            )?;

            // 3. WINDOW_UPDATE on connection (stream 0)
            h2::write_window_update(&mut *w, 0, 983_041)?;

            // 4. Open stream 1 (CS) and stream 3 (SC) with empty HEADERS
            h2::write_headers(&mut *w, STREAM_CS)?;
            h2::write_headers(&mut *w, STREAM_SC)?;

            // 5. XPC init frame A on CS: flags=ALWAYS_SET (0x1), no body
            let init_a = encode_message(&Message::init(flags::ALWAYS_SET));
            h2::write_data(&mut *w, STREAM_CS, &init_a)?;

            // 6. XPC init frame B on CS: flags=0x201 (ALWAYS_SET | 0x200)
            let init_b = encode_message(&Message::init(flags::ALWAYS_SET | 0x200));
            h2::write_data(&mut *w, STREAM_CS, &init_b)?;

            // 7. XPC init frame C on SC: flags=INIT_HANDSHAKE | ALWAYS_SET (0x400001)
            let init_c = encode_message(&Message::init(flags::INIT_HANDSHAKE | flags::ALWAYS_SET));
            h2::write_data(&mut *w, STREAM_SC, &init_c)?;

            w.flush()?;
        }

        // 8. Wait for server SETTINGS; reply with ACK
        {
            let mut r = self.reader.lock().unwrap();
            loop {
                match h2::read_frame(&mut *r)? {
                    Frame::Settings { flags: f, .. } if f & h2::FLAG_ACK == 0 => {
                        drop(r);
                        let mut w = self.writer.lock().unwrap();
                        h2::write_settings_ack(&mut *w)?;
                        w.flush()?;
                        break;
                    }
                    // Some implementations send ACK first; accept it as completion too
                    Frame::Settings { .. } => break,
                    _ => continue,
                }
            }
        }

        Ok(())
    }
}
