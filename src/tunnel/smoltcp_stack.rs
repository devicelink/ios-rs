/// Userspace IPv6 TCP/IP stack backed by a CDTunnel connection.
///
/// Two constructors:
///   - `new(CdTunnelConn)` — plain TCP; uses separate reader + poll threads.
///   - `new_stream(impl Read+Write, TunnelParams)` — generic (e.g. TLS);
///     uses a single unified I/O+poll thread with a short read timeout.
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::Ipv6Addr;
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv6Address, Ipv6Cidr};

use super::cdtunnel::{CdTunnelConn, TunnelParams, recv_ipv6};
use super::error::Error;

// ── public API ────────────────────────────────────────────────────────────────

pub struct SmoltcpTunnel {
    pub params:   TunnelParams,
    connect_tx:   mpsc::SyncSender<ConnectReq>,
    _threads:     Vec<thread::JoinHandle<()>>,
}

impl SmoltcpTunnel {
    /// Build a tunnel from a plain-TCP CDTunnel connection.
    /// Uses a dedicated reader thread + a poll thread.
    pub fn new(cdtunnel: CdTunnelConn) -> Result<Self, Error> {
        let params     = cdtunnel.params.clone();
        let rx_queue: Arc<Mutex<VecDeque<Vec<u8>>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (connect_tx, connect_rx) = mpsc::sync_channel::<ConnectReq>(32);

        let rx_q          = Arc::clone(&rx_queue);
        let mut reader_s  = cdtunnel.try_clone_stream()
            .map_err(|e| Error::Protocol(format!("clone stream: {e}")))?;
        let reader_thread = thread::Builder::new()
            .name("cdtunnel-reader".into())
            .spawn(move || {
                while let Ok(pkt) = recv_ipv6(&mut reader_s) {
                    rx_q.lock().unwrap().push_back(pkt);
                }
            })
            .map_err(|e| Error::Protocol(format!("reader thread: {e}")))?;

        let client_addr = params.client_addr;
        let mtu         = params.mtu as usize;
        let rx_q2       = Arc::clone(&rx_queue);
        let poll_thread = thread::Builder::new()
            .name("smoltcp-poll".into())
            .spawn(move || poll_loop(cdtunnel, client_addr, mtu, rx_q2, connect_rx))
            .map_err(|e| Error::Protocol(format!("poll thread: {e}")))?;

        Ok(SmoltcpTunnel {
            params,
            connect_tx,
            _threads: vec![reader_thread, poll_thread],
        })
    }

    /// Build a tunnel from any `Read + Write` stream (e.g. TLS-wrapped TCP).
    ///
    /// The stream **must** have a short read timeout set on the underlying
    /// socket before calling this (≤ 5 ms) so that the unified loop can
    /// alternate between reads and smoltcp polling without blocking.
    pub fn new_stream<S: Read + Write + Send + 'static>(
        stream: S,
        params: TunnelParams,
    ) -> Result<Self, Error> {
        let (connect_tx, connect_rx) = mpsc::sync_channel::<ConnectReq>(32);
        let client_addr = params.client_addr;
        let mtu         = params.mtu as usize;

        let poll_thread = thread::Builder::new()
            .name("smoltcp-unified".into())
            .spawn(move || unified_loop(stream, client_addr, mtu, connect_rx))
            .map_err(|e| Error::Protocol(format!("unified thread: {e}")))?;

        Ok(SmoltcpTunnel {
            params,
            connect_tx,
            _threads: vec![poll_thread],
        })
    }

    /// Open a TCP connection to `addr:port` through the tunnel.
    /// Blocks until TCP ESTABLISHED. Returns a `UnixStream` byte pipe.
    pub fn connect(&self, addr: Ipv6Addr, port: u16) -> Result<UnixStream, Error> {
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.connect_tx
            .send(ConnectReq { addr, port, result_tx })
            .map_err(|_| Error::Protocol("smoltcp poll thread gone".into()))?;
        result_rx
            .recv()
            .map_err(|_| Error::Protocol("smoltcp poll thread gone".into()))?
            .map_err(Error::Protocol)
    }
}

// ── poll loop (plain TCP) ─────────────────────────────────────────────────────

struct ConnectReq {
    addr:      Ipv6Addr,
    port:      u16,
    result_tx: mpsc::SyncSender<Result<UnixStream, String>>,
}

fn now() -> Instant {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Instant::from_millis(ms)
}

fn poll_loop(
    mut cdtunnel: CdTunnelConn,
    client_addr:  Ipv6Addr,
    mtu:          usize,
    rx_queue:     Arc<Mutex<VecDeque<Vec<u8>>>>,
    connect_rx:   mpsc::Receiver<ConnectReq>,
) {
    let mut device = TunnelDevice { rx: VecDeque::new(), tx: VecDeque::new(), mtu };
    let config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(config, &mut device, now());
    let a = Ipv6Address::from_bytes(&client_addr.octets());
    iface.update_ip_addrs(|addrs| { let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(a, 64))); });

    let mut sockets   = SocketSet::new(vec![]);
    let mut pending:  Vec<(SocketHandle, mpsc::SyncSender<Result<UnixStream, String>>)> = Vec::new();
    let mut active:   Vec<(SocketHandle, UnixStream, Vec<u8>)> = Vec::new();
    let mut next_port: u16 = 49152;

    loop {
        { let mut q = rx_queue.lock().unwrap(); device.rx.extend(q.drain(..)); }

        while let Ok(req) = connect_rx.try_recv() {
            let port = next_port;
            next_port = next_port.wrapping_add(1).max(49152);
            let mut sock = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0u8; 524288]),
                tcp::SocketBuffer::new(vec![0u8; 524288]),
            );
            let remote = (Ipv6Address::from_bytes(&req.addr.octets()), req.port);
            match sock.connect(iface.context(), remote, port) {
                Ok(()) => { pending.push((sockets.add(sock), req.result_tx)); }
                Err(e) => { let _ = req.result_tx.send(Err(format!("{e:?}"))); }
            }
        }

        let _ = iface.poll(now(), &mut device, &mut sockets);

        while let Some(pkt) = device.tx.pop_front() {
            let _ = cdtunnel.send_ipv6_packet(&pkt);
        }

        promote_and_bridge(&mut sockets, &mut pending, &mut active);
        thread::sleep(Duration::from_millis(1));
    }
}

// ── unified loop (generic Read+Write, e.g. TLS) ───────────────────────────────

fn unified_loop<S: Read + Write>(
    mut stream:  S,
    client_addr: Ipv6Addr,
    mtu:         usize,
    connect_rx:  mpsc::Receiver<ConnectReq>,
) {
    let mut device = TunnelDevice { rx: VecDeque::new(), tx: VecDeque::new(), mtu };
    let config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(config, &mut device, now());
    let a = Ipv6Address::from_bytes(&client_addr.octets());
    iface.update_ip_addrs(|addrs| { let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(a, 64))); });

    let mut sockets   = SocketSet::new(vec![]);
    let mut pending:  Vec<(SocketHandle, mpsc::SyncSender<Result<UnixStream, String>>)> = Vec::new();
    let mut active:   Vec<(SocketHandle, UnixStream, Vec<u8>)> = Vec::new();
    let mut next_port: u16 = 49152;

    loop {
        // Non-blocking read attempt (short timeout set on underlying socket)
        match recv_ipv6(&mut stream) {
            Ok(pkt)                                               => { device.rx.push_back(pkt); }
            Err(e) if e.kind() == ErrorKind::WouldBlock           => {}
            Err(e) if e.kind() == ErrorKind::TimedOut             => {}
            Err(_)                                                => return,
        }

        while let Ok(req) = connect_rx.try_recv() {
            let port = next_port;
            next_port = next_port.wrapping_add(1).max(49152);
            let mut sock = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0u8; 524288]),
                tcp::SocketBuffer::new(vec![0u8; 524288]),
            );
            let remote = (Ipv6Address::from_bytes(&req.addr.octets()), req.port);
            match sock.connect(iface.context(), remote, port) {
                Ok(()) => { pending.push((sockets.add(sock), req.result_tx)); }
                Err(e) => { let _ = req.result_tx.send(Err(format!("{e:?}"))); }
            }
        }

        let _ = iface.poll(now(), &mut device, &mut sockets);

        while let Some(pkt) = device.tx.pop_front() {
            if stream.write_all(&pkt).is_err() { return; }
        }
        let _ = stream.flush();

        promote_and_bridge(&mut sockets, &mut pending, &mut active);
    }
}

// ── shared socket management ──────────────────────────────────────────────────

fn promote_and_bridge(
    sockets: &mut SocketSet<'_>,
    pending: &mut Vec<(SocketHandle, mpsc::SyncSender<Result<UnixStream, String>>)>,
    active:  &mut Vec<(SocketHandle, UnixStream, Vec<u8>)>,
) {
    pending.retain(|(handle, result_tx)| {
        let sock = sockets.get_mut::<tcp::Socket>(*handle);
        match sock.state() {
            tcp::State::Established => {
                match UnixStream::pair() {
                    Ok((local, proxy)) => {
                        proxy.set_nonblocking(true).ok();
                        active.push((*handle, proxy, Vec::new()));
                        let _ = result_tx.send(Ok(local));
                    }
                    Err(e) => { let _ = result_tx.send(Err(e.to_string())); }
                }
                false
            }
            tcp::State::Closed | tcp::State::TimeWait => {
                let _ = result_tx.send(Err("connection refused or timed out".into()));
                false
            }
            _ => true,
        }
    });

    let mut i = 0;
    while i < active.len() {
        let (handle, proxy, write_buf) = &mut active[i];
        let sock = sockets.get_mut::<tcp::Socket>(*handle);

        // Flush any previously buffered data before reading more from smoltcp.
        if !write_buf.is_empty() {
            match proxy.write_all(write_buf) {
                Ok(()) => write_buf.clear(),
                Err(_) => {
                    // Pipe still full — skip smoltcp read; try again next poll.
                    i += 1;
                    continue;
                }
            }
        }

        // Forward data from smoltcp socket → UnixStream pipe.
        // If the pipe is full, buffer the unwritten bytes so smoltcp data is not lost.
        while sock.can_recv() {
            let mut buf = [0u8; 8192];
            match sock.recv_slice(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Err(_) = proxy.write_all(&buf[..n]) {
                        // Pipe full: save for next poll iteration.
                        write_buf.extend_from_slice(&buf[..n]);
                        break;
                    }
                }
            }
        }

        // Forward data from UnixStream pipe → smoltcp socket (outgoing).
        while sock.can_send() {
            let mut buf = [0u8; 8192];
            match proxy.read(&mut buf) {
                Ok(0)                                              => { sock.close(); break; }
                Ok(n)                                              => { let _ = sock.send_slice(&buf[..n]); }
                Err(e) if e.kind() == ErrorKind::WouldBlock        => break,
                Err(_)                                             => { sock.close(); break; }
            }
        }

        if matches!(sock.state(), tcp::State::Closed | tcp::State::TimeWait) && !sock.can_recv() {
            sockets.remove(*handle);
            active.remove(i);
            continue;
        }
        i += 1;
    }
}

// ── smoltcp Device ────────────────────────────────────────────────────────────

struct TunnelDevice {
    rx:  VecDeque<Vec<u8>>,
    tx:  VecDeque<Vec<u8>>,
    mtu: usize,
}

impl Device for TunnelDevice {
    type RxToken<'a> = IpRxToken where Self: 'a;
    type TxToken<'a> = IpTxToken<'a> where Self: 'a;

    fn receive(&mut self, _: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.rx.pop_front()?;
        Some((IpRxToken(pkt), IpTxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Self::TxToken<'_>> {
        Some(IpTxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ip;
        c.max_transmission_unit = self.mtu;
        c
    }
}

struct IpRxToken(Vec<u8>);
impl RxToken for IpRxToken {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(mut self, f: F) -> R { f(&mut self.0) }
}

struct IpTxToken<'a>(&'a mut VecDeque<Vec<u8>>);
impl<'a> TxToken for IpTxToken<'a> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.push_back(buf);
        r
    }
}
