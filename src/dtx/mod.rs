/// DTX protocol — Apple's binary RPC transport used by testmanagerd and Instruments.
///
/// Wire format (all integers little-endian except the 4-byte magic which is big-endian):
///
/// Message header (32 bytes):
///   [0-3]   magic = 0x795B3D1F  (big-endian on the wire: 0x79 0x5B 0x3D 0x1F)
///   [4-7]   header_size = 32
///   [8-9]   fragment_index
///   [10-11] fragment_count
///   [12-15] message_length  (payload header + optional aux header + aux entries + payload)
///   [16-19] message_id
///   [20-23] conversation_id (0 for requests, == msg_id of the request for replies)
///   [24-27] channel_code
///   [28-31] expects_reply (0 or 1)
///
/// Payload header (16 bytes, always present when message_length > 0):
///   [0-3]   message_type  (2=MethodInvocation, 3=Reply, 0=Ack, 4=Error)
///   [4-7]   aux_length    (= 16 + len(aux_entries) if aux present, else 0)
///   [8-11]  total_length  (= aux_length + len(payload_bytes))
///   [12-15] flags
///
/// Auxiliary header (16 bytes, present iff aux_length > 0):
///   [0-3]   0x1F0 (magic constant)
///   [4-7]   0
///   [8-11]  aux_entries_size  (= len(aux_entries))
///   [12-15] 0
///
/// Auxiliary entries (aux_entries_size bytes):
///   Each argument:  [t_null=0x0A][value_type][optional_size][data]
///   value_type 0x03 = uint32 (4 bytes, no size prefix)
///   value_type 0x06 = uint64 (8 bytes, no size prefix)
///   value_type 0x02 = bytes  (u32 size + data)
///
/// Payload bytes (total_length - aux_length bytes):
///   NSKeyedArchiver binary plist.
use std::collections::HashMap;
use std::io::{BufWriter, Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O: {0}")]   Io(#[from] std::io::Error),
    #[error("plist: {0}")] Plist(#[from] plist::Error),
    #[error("bad DTX magic: {0:#010x}")] BadMagic(u32),
    #[error("protocol: {0}")] Protocol(String),
    #[error("DTX connection closed")] Closed,
}

// ── message types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MsgType {
    Ack             = 0,
    Unknown1        = 1,
    MethodInvoke    = 2,
    Reply           = 3,
    Error           = 4,
}

impl TryFrom<u32> for MsgType {
    type Error = u32;
    fn try_from(v: u32) -> Result<Self, u32> {
        match v {
            0 => Ok(Self::Ack),
            1 => Ok(Self::Unknown1),
            2 => Ok(Self::MethodInvoke),
            3 => Ok(Self::Reply),
            4 => Ok(Self::Error),
            n => Err(n),
        }
    }
}

// ── public message type ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DtxMessage {
    pub msg_id:  u32,
    pub conv_id: u32,
    pub channel: i32,
    pub expects_reply: bool,
    pub msg_type: u32,
    pub aux: Vec<AuxValue>,
    pub payload: Option<plist::Value>,
}

#[derive(Debug, Clone)]
pub enum AuxValue {
    Int32(i32),
    Int64(i64),
    Bytes(Vec<u8>),
}

// ── encoding ──────────────────────────────────────────────────────────────────

const MAGIC: [u8; 4] = [0x79, 0x5B, 0x3D, 0x1F];
const T_NULL:     u32 = 0x0A;
const T_UINT32:   u32 = 0x03;
const T_INT64:    u32 = 0x06;
const T_BYTEARRAY:u32 = 0x02;
const AUX_MAGIC:  u32 = 0x1F0;

/// Encode a DTX method-invocation message.
pub fn encode_call(
    msg_id:  u32,
    conv_id: u32,
    channel: i32,
    expects_reply: bool,
    selector: &str,
    aux: &[AuxValue],
) -> Vec<u8> {
    let payload = archive_string(selector);
    encode_raw(msg_id, conv_id, channel, expects_reply, MsgType::MethodInvoke, aux, &payload)
}

/// Encode an ACK reply to an incoming message.
/// go-ios ACK format: 32-byte header (messageLength=16) + 16-byte empty payload header.
pub fn encode_ack(msg: &DtxMessage) -> Vec<u8> {
    let mut buf = vec![0u8; 48];
    // Header (32 bytes) — magic big-endian, rest little-endian
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4..8].copy_from_slice(&32u32.to_le_bytes());
    buf[8..10].copy_from_slice(&0u16.to_le_bytes());   // fragment_index
    buf[10..12].copy_from_slice(&1u16.to_le_bytes());  // fragment_count
    buf[12..16].copy_from_slice(&16u32.to_le_bytes()); // message_length = 16 (payload header only)
    buf[16..20].copy_from_slice(&msg.msg_id.to_le_bytes());
    buf[20..24].copy_from_slice(&(msg.conv_id + 1).to_le_bytes());
    buf[24..28].copy_from_slice(&(msg.channel as u32).to_le_bytes());
    buf[28..32].copy_from_slice(&0u32.to_le_bytes()); // expects_reply = false
    // Payload header (16 bytes) — message_type=Ack, everything else 0
    buf[32..36].copy_from_slice(&(MsgType::Ack as u32).to_le_bytes());
    // bytes 36..48 stay zero
    buf
}

/// Encode a reply with a plist payload.
pub fn encode_reply(req: &DtxMessage, payload_bytes: &[u8]) -> Vec<u8> {
    encode_raw(req.msg_id, req.conv_id + 1, req.channel, false, MsgType::Reply, &[], payload_bytes)
}

fn encode_raw(
    msg_id:  u32,
    conv_id: u32,
    channel: i32,
    expects_reply: bool,
    msg_type: MsgType,
    aux: &[AuxValue],
    payload: &[u8],
) -> Vec<u8> {
    let aux_entries = encode_aux_entries(aux);
    let aux_entry_len = aux_entries.len();
    let aux_len_with_hdr = if aux_entry_len > 0 { aux_entry_len + 16 } else { 0 };
    let total_payload   = aux_len_with_hdr + payload.len();
    let message_len     = if total_payload > 0 { 16 + total_payload } else { 0 };

    let mut buf = Vec::with_capacity(32 + message_len);
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&32u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());  // fragment_index
    buf.extend_from_slice(&1u16.to_le_bytes());  // fragment_count
    buf.extend_from_slice(&(message_len as u32).to_le_bytes());
    buf.extend_from_slice(&msg_id.to_le_bytes());
    buf.extend_from_slice(&conv_id.to_le_bytes());
    buf.extend_from_slice(&(channel as u32).to_le_bytes());
    buf.extend_from_slice(&(expects_reply as u32).to_le_bytes());

    if message_len == 0 { return buf; }

    // Payload header
    buf.extend_from_slice(&(msg_type as u32).to_le_bytes());
    buf.extend_from_slice(&(aux_len_with_hdr as u32).to_le_bytes());
    buf.extend_from_slice(&(total_payload as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // flags

    if aux_entry_len > 0 {
        buf.extend_from_slice(&AUX_MAGIC.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&(aux_entry_len as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&aux_entries);
    }
    buf.extend_from_slice(payload);
    buf
}

fn encode_aux_entries(args: &[AuxValue]) -> Vec<u8> {
    let mut buf = Vec::new();
    for arg in args {
        buf.extend_from_slice(&T_NULL.to_le_bytes());
        match arg {
            AuxValue::Int32(v) => {
                buf.extend_from_slice(&T_UINT32.to_le_bytes());
                buf.extend_from_slice(&(*v as u32).to_le_bytes());
            }
            AuxValue::Int64(v) => {
                buf.extend_from_slice(&T_INT64.to_le_bytes());
                buf.extend_from_slice(&(*v as u64).to_le_bytes());
            }
            AuxValue::Bytes(b) => {
                buf.extend_from_slice(&T_BYTEARRAY.to_le_bytes());
                buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
                buf.extend_from_slice(b);
            }
        }
    }
    buf
}

// ── NSKeyedArchiver primitives (used here and in xctest) ──────────────────────

/// Archive a single plist primitive (string, integer, bool, data) directly.
/// Per go-ios: primitive types go straight into $objects without wrapping.
pub fn archive_primitive(v: plist::Value) -> Vec<u8> {
    let mut top = plist::Dictionary::new();
    top.insert("root".into(), plist::Value::Uid(plist::Uid::new(1)));
    let mut d = plist::Dictionary::new();
    d.insert("$version".into(),  plist::Value::Integer(100000.into()));
    d.insert("$archiver".into(), plist::Value::String("NSKeyedArchiver".into()));
    d.insert("$top".into(),      plist::Value::Dictionary(top));
    d.insert("$objects".into(),  plist::Value::Array(vec![
        plist::Value::String("$null".into()),
        v,
    ]));
    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &plist::Value::Dictionary(d)).unwrap();
    buf
}

pub fn archive_string(s: &str) -> Vec<u8> {
    archive_primitive(plist::Value::String(s.into()))
}

pub fn archive_u64(n: u64) -> Vec<u8> {
    archive_primitive(plist::Value::Integer(n.into()))
}

pub fn archive_bool(b: bool) -> Vec<u8> {
    archive_primitive(plist::Value::Boolean(b))
}

/// NSKeyedArchive the minimal capabilities dict expected by testmanagerd:
/// `{"com.apple.private.DTXBlockCompression": 0, "com.apple.private.DTXConnection": 1}`
pub fn archive_capabilities_dict() -> Vec<u8> {
    let mut objects: Vec<plist::Value> = vec![plist::Value::String("$null".into())];

    let class_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::Dictionary({
        let mut d = plist::Dictionary::new();
        d.insert("$classname".into(), plist::Value::String("NSDictionary".into()));
        d.insert("$classes".into(), plist::Value::Array(vec![
            plist::Value::String("NSDictionary".into()),
            plist::Value::String("NSObject".into()),
        ]));
        d
    }));

    let k1_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::String("com.apple.private.DTXBlockCompression".into()));
    let k2_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::String("com.apple.private.DTXConnection".into()));
    let v1_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::Integer(0.into()));
    let v2_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::Integer(1.into()));

    let dict_uid = plist::Uid::new(objects.len() as u64);
    objects.push(plist::Value::Dictionary({
        let mut d = plist::Dictionary::new();
        d.insert("NS.keys".into(), plist::Value::Array(vec![
            plist::Value::Uid(k1_uid), plist::Value::Uid(k2_uid),
        ]));
        d.insert("NS.objects".into(), plist::Value::Array(vec![
            plist::Value::Uid(v1_uid), plist::Value::Uid(v2_uid),
        ]));
        d.insert("$class".into(), plist::Value::Uid(class_uid));
        d
    }));

    let mut top = plist::Dictionary::new();
    top.insert("root".into(), plist::Value::Uid(dict_uid));
    let mut root = plist::Dictionary::new();
    root.insert("$version".into(),  plist::Value::Integer(100000.into()));
    root.insert("$archiver".into(), plist::Value::String("NSKeyedArchiver".into()));
    root.insert("$top".into(),      plist::Value::Dictionary(top));
    root.insert("$objects".into(),  plist::Value::Array(objects));
    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &plist::Value::Dictionary(root)).unwrap();
    buf
}

// ── decoding ──────────────────────────────────────────────────────────────────

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read(&mut buf[done..])?;
        if n == 0 { return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "dtx eof")); }
        done += n;
    }
    Ok(())
}

pub fn read_message<R: Read>(r: &mut R) -> Result<DtxMessage, Error> {
    let mut hdr = [0u8; 32];
    read_exact(r, &mut hdr)?;

    // Magic is big-endian
    let magic = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
    if magic != 0x795B3D1F { return Err(Error::BadMagic(magic)); }

    let msg_len   = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
    let msg_id    = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
    let conv_id   = u32::from_le_bytes(hdr[20..24].try_into().unwrap());
    let channel   = i32::from_le_bytes(hdr[24..28].try_into().unwrap());
    let expects   = u32::from_le_bytes(hdr[28..32].try_into().unwrap()) != 0;

    if msg_len == 0 {
        return Ok(DtxMessage { msg_id, conv_id, channel, expects_reply: expects, msg_type: 0, aux: vec![], payload: None });
    }

    let mut payload = vec![0u8; msg_len];
    read_exact(r, &mut payload)?;

    let msg_type  = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    let aux_len   = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let total     = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
    // payload[12..16] = flags (ignored)

    let aux = if aux_len > 0 {
        // aux header (16 bytes) + aux entries
        let entries_size = aux_len.saturating_sub(16);
        let entries_start = 16 + 16; // payload header + aux header
        if payload.len() >= entries_start + entries_size {
            decode_aux_entries(&payload[entries_start..entries_start + entries_size])
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let body_start = 16 + aux_len;
    let body_len   = total.saturating_sub(aux_len);
    let body = if body_len > 0 && payload.len() >= body_start + body_len {
        let b = &payload[body_start..body_start + body_len];
        Some(plist::from_bytes::<plist::Value>(b)?)
    } else {
        None
    };

    Ok(DtxMessage { msg_id, conv_id, channel, expects_reply: expects, msg_type, aux, payload: body })
}

fn decode_aux_entries(data: &[u8]) -> Vec<AuxValue> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let key_type = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap());
        pos += 4;
        if key_type != T_NULL { break; } // unexpected
        if pos + 4 > data.len() { break; }
        let val_type = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap());
        pos += 4;
        match val_type {
            T_UINT32 if pos + 4 <= data.len() => {
                let v = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap());
                out.push(AuxValue::Int32(v as i32));
                pos += 4;
            }
            T_INT64 if pos + 8 <= data.len() => {
                let v = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap());
                out.push(AuxValue::Int64(v as i64));
                pos += 8;
            }
            T_BYTEARRAY if pos + 4 <= data.len() => {
                let len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
                pos += 4;
                if pos + len <= data.len() {
                    out.push(AuxValue::Bytes(data[pos..pos+len].to_vec()));
                    pos += len;
                } else { break; }
            }
            _ => break,
        }
    }
    out
}

// ── blocking connection ───────────────────────────────────────────────────────

type WaiterMap  = Arc<Mutex<HashMap<u32, mpsc::SyncSender<DtxMessage>>>>;
type DispatchMap = Arc<Mutex<HashMap<i32, mpsc::SyncSender<DtxMessage>>>>;

/// A blocking DTX connection.
///
/// Starts a reader thread that demultiplexes messages:
/// - replies (conv_id > 0) → forwarded to the matching `call()` waiter
/// - incoming method invocations → forwarded to the channel's dispatch queue
pub struct DtxConn {
    writer:       Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>,
    next_id:      Arc<Mutex<u32>>,
    next_chan:    Arc<Mutex<i32>>,
    waiters:      WaiterMap,
    dispatchers:  DispatchMap,
    _reader:      thread::JoinHandle<()>,
    timeout:      Duration,
}

impl DtxConn {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let waiters:     WaiterMap   = Arc::new(Mutex::new(HashMap::new()));
        let dispatchers: DispatchMap = Arc::new(Mutex::new(HashMap::new()));
        let w = Arc::new(Mutex::new(BufWriter::new(Box::new(writer) as Box<dyn Write + Send>)));

        let w2 = Arc::clone(&w);
        let wt = Arc::clone(&waiters);
        let dt = Arc::clone(&dispatchers);
        let handle = thread::Builder::new()
            .name("dtx-reader".into())
            .spawn(move || reader_loop(reader, w2, wt, dt))
            .expect("dtx reader thread");

        DtxConn { writer: w, next_id: Arc::new(Mutex::new(1)), next_chan: Arc::new(Mutex::new(1)), waiters, dispatchers, _reader: handle, timeout: Duration::from_secs(10) }
    }

    /// Send a method call and wait for the reply. Returns the reply's payload.
    pub fn call(
        &self,
        channel: i32,
        selector: &str,
        aux: &[AuxValue],
    ) -> Result<Option<plist::Value>, Error> {
        let id   = self.alloc_id();
        let (tx, rx) = mpsc::sync_channel(1);
        self.waiters.lock().unwrap().insert(id, tx);

        let frame = encode_call(id, 0, channel, true, selector, aux);
        self.write_bytes(&frame)?;

        rx.recv_timeout(self.timeout)
            .map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout      => Error::Protocol(format!("DTX call '{selector}' timed out")),
                mpsc::RecvTimeoutError::Disconnected => Error::Closed,
            })
            .map(|m| m.payload)
    }

    /// Send a method call without waiting for a reply.
    pub fn call_async(&self, channel: i32, selector: &str, aux: &[AuxValue]) -> Result<(), Error> {
        let id    = self.alloc_id();
        let frame = encode_call(id, 0, channel, false, selector, aux);
        self.write_bytes(&frame)
    }

    /// Perform the DTX capability handshake required by testmanagerd.
    ///
    /// Must be called once per connection before any `request_channel` call.
    /// Sends `_notifyOfPublishedCapabilities:` (no reply expected) with the
    /// minimal capabilities dict observed in both pymd3 and go-ios.
    pub fn handshake(&self) -> Result<(), Error> {
        let caps = archive_capabilities_dict();
        self.call_async(0, "_notifyOfPublishedCapabilities:", &[AuxValue::Bytes(caps)])
    }

    /// Request a named DTX channel. Returns the channel code to use in subsequent calls.
    ///
    /// Per go-ios: ALL method call arguments must be NSKeyedArchiver-encoded (t_bytearray),
    /// including primitive integers. The channel code is allocated locally and the server
    /// echoes it back; we return the locally-allocated code, not the echo.
    pub fn request_channel(&self, identifier: &str) -> Result<i32, Error> {
        let code = {
            let mut c = self.next_chan.lock().unwrap();
            let v = *c;
            *c += 1;
            v
        };
        let aux = vec![
            // Channel code sent as raw PInt32 (pymd3 convention)
            AuxValue::Int32(code),
            AuxValue::Bytes(archive_string(identifier)),
        ];
        self.call(0, "_requestChannelWithCode:identifier:", &aux)?;
        Ok(code)
    }

    /// Register a channel to receive incoming method calls.
    /// Returns a Receiver that yields incoming DtxMessages.
    pub fn register_channel(&self, channel: i32) -> mpsc::Receiver<DtxMessage> {
        let (tx, rx) = mpsc::sync_channel(32);
        self.dispatchers.lock().unwrap().insert(channel, tx);
        rx
    }

    /// Send a reply to an incoming message.
    pub fn reply(&self, req: &DtxMessage, payload: &[u8]) -> Result<(), Error> {
        let frame = encode_reply(req, payload);
        self.write_bytes(&frame)
    }

    fn alloc_id(&self) -> u32 {
        let mut id = self.next_id.lock().unwrap();
        let v = *id;
        *id = id.wrapping_add(1).max(1);
        v
    }

    fn write_bytes(&self, data: &[u8]) -> Result<(), Error> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }
}

fn reader_loop<R: Read>(
    mut reader:     R,
    writer:         Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>,
    waiters:        WaiterMap,
    dispatchers:    DispatchMap,
) {
    loop {
        let msg = match read_message(&mut reader) {
            Ok(m)  => m,
            Err(_) => return,
        };

        // ACKs: send ack if requested, then ignore
        if msg.msg_type == MsgType::Ack as u32 { continue; }

        // Reply to a waiting call
        if msg.conv_id > 0 {
            if let Some(tx) = waiters.lock().unwrap().remove(&msg.msg_id) {
                let _ = tx.send(msg);
            }
            continue;
        }

        // Incoming method invocation or other — dispatch to channel handler
        if msg.expects_reply {
            // Send ACK immediately
            let ack = encode_ack(&msg);
            if let Ok(mut w) = writer.lock() {
                let _ = w.write_all(&ack);
                let _ = w.flush();
            }
        }

        if let Some(tx) = dispatchers.lock().unwrap().get(&msg.channel) {
            let _ = tx.send(msg);
        }
    }
}
