/// RSD tunnel daemon: one background process that holds CDTunnel connections
/// open for all connected iOS 17.4+ devices and proxies service connections to
/// CLI commands on demand.
///
/// Socket protocol (4-byte BE length + XML plist, both directions):
///
///   Request  → {UDID: str, Service: str}
///   Response → {Status: "Ok"}            — connection is now a raw byte pipe
///              {Status: "Error", Error: str}
///
///   Special services
///     "_rsd"  — raw proxy to the device's RSD port (58783); no RSDCheckin
///
///   Control requests (no UDID/Service)
///     {Request: "List"}     → {Status: "Ok", Devices: [{UDID, ProductType, OSVersion,
///                                                        DeviceName, ECID, Services}…]}
///     {Request: "Watch"}    → stream of {Event: "Attached", UDID, ProductType, OSVersion,
///                                                            DeviceName, ECID, Services}
///                                                           {Event: "Detached", UDID}
///                             (bootstrapped with current tunnels; connection stays open)
///     {Request: "Shutdown"} → daemon exits
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::cmd::output::{print_json, ActionResult, OutputMode};
use anyhow::{bail, Context, Result};
use ios_rs::lockdown::LockdownSession;
use ios_rs::rsd::{PeerInfo, ServiceEntry};
use ios_rs::tunnel::{detect_version, Ios17Tunnel, SmoltcpTunnel, DAEMON_SOCKET};
use ios_rs::usbmux::{Connection as MuxConn, Event, MuxSocket};

const PID_PATH: &str = "/tmp/ios-rsd.pid";

// ── listen address ────────────────────────────────────────────────────────────

enum ListenAddr {
    Unix(String),
    Tcp(std::net::SocketAddr),
}

fn parse_listen_addr(s: &str) -> Result<ListenAddr> {
    if let Some(path) = s.strip_prefix("unix://") {
        Ok(ListenAddr::Unix(path.to_owned()))
    } else if let Some(hostport) = s.strip_prefix("tcp://") {
        let addr: std::net::SocketAddr = hostport
            .parse()
            .with_context(|| format!("invalid TCP address: {hostport}"))?;
        Ok(ListenAddr::Tcp(addr))
    } else {
        bail!("listen address must start with unix:// or tcp://: {s}")
    }
}

fn resolve_listen_addr(flag: Option<&str>) -> Result<ListenAddr> {
    if let Some(s) = flag {
        return parse_listen_addr(s);
    }
    if let Ok(s) = std::env::var(ios_rs::tunnel::DAEMON_SOCKET_ENV) {
        return parse_listen_addr(&s);
    }
    Ok(ListenAddr::Unix(DAEMON_SOCKET.to_owned()))
}

fn probe_daemon(addr: &ListenAddr) -> bool {
    match addr {
        ListenAddr::Unix(path) => UnixStream::connect(path).is_ok(),
        ListenAddr::Tcp(a) => TcpStream::connect(a).is_ok(),
    }
}

fn connect_daemon(addr: &ListenAddr) -> Result<MuxSocket> {
    match addr {
        ListenAddr::Unix(path) => UnixStream::connect(path)
            .map(MuxSocket::Unix)
            .context("daemon not running"),
        ListenAddr::Tcp(a) => TcpStream::connect(a)
            .map(MuxSocket::Tcp)
            .context("daemon not running"),
    }
}

// ── shared state ──────────────────────────────────────────────────────────────

struct DeviceTunnel {
    tunnel: Arc<SmoltcpTunnel>,
    services: HashMap<String, ServiceEntry>,
    device_id: u32,
    peer_info: PeerInfo,
    device_name: String,
    ecid: u64,
}

#[derive(Clone)]
enum WatchEvent {
    Attached(plist::Dictionary),
    Detached { udid: String },
}

struct Shared {
    tunnels: HashMap<String, DeviceTunnel>,
    watchers: Vec<mpsc::Sender<WatchEvent>>,
}

type State = Arc<Mutex<Shared>>;

fn broadcast_locked(s: &mut Shared, event: WatchEvent) {
    s.watchers.retain(|tx| tx.send(event.clone()).is_ok());
}

fn tunnel_plist_dict(udid: &str, dt: &DeviceTunnel) -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    d.insert("UDID".into(), plist::Value::String(udid.to_owned()));
    d.insert(
        "ProductType".into(),
        plist::Value::String(dt.peer_info.product_type.clone()),
    );
    d.insert(
        "OSVersion".into(),
        plist::Value::String(dt.peer_info.os_version.clone()),
    );
    d.insert(
        "DeviceName".into(),
        plist::Value::String(dt.device_name.clone()),
    );
    d.insert(
        "ECID".into(),
        plist::Value::Integer(plist::Integer::from(dt.ecid as i64)),
    );
    d.insert(
        "Services".into(),
        plist::Value::Integer(plist::Integer::from(dt.services.len() as i64)),
    );
    d
}

// ── public CLI entry-points ───────────────────────────────────────────────────

/// Run the daemon process (blocks until told to shut down).
pub fn daemon(listen: Option<String>) -> Result<()> {
    let _ = std::fs::write(PID_PATH, std::process::id().to_string());

    let addr = resolve_listen_addr(listen.as_deref())?;
    let socket_path = match &addr {
        ListenAddr::Unix(p) => {
            let _ = std::fs::remove_file(p);
            Some(p.clone())
        }
        ListenAddr::Tcp(_) => None,
    };

    let state: State = Arc::new(Mutex::new(Shared {
        tunnels: HashMap::new(),
        watchers: Vec::new(),
    }));

    let watcher_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("usbmux-watcher".into())
        .spawn(move || watch_devices(watcher_state))
        .context("spawn watcher thread")?;

    match addr {
        ListenAddr::Unix(ref path) => {
            let listener = UnixListener::bind(path).with_context(|| format!("bind {path}"))?;
            eprintln!(
                "[ios-rsd] started  pid={}  socket={path}",
                std::process::id()
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let sock = MuxSocket::Unix(stream);
                let state = Arc::clone(&state);
                let sp = socket_path.clone();
                std::thread::Builder::new()
                    .name("ipc-client".into())
                    .spawn(move || {
                        if let Err(e) = handle_client(sock, &state, sp.as_deref()) {
                            eprintln!("[ios-rsd] client error: {e:#}");
                        }
                    })
                    .ok();
            }
        }
        ListenAddr::Tcp(addr) => {
            let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
            eprintln!("[ios-rsd] started  pid={}  tcp={addr}", std::process::id());
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let sock = MuxSocket::Tcp(stream);
                let state = Arc::clone(&state);
                std::thread::Builder::new()
                    .name("ipc-client".into())
                    .spawn(move || {
                        if let Err(e) = handle_client(sock, &state, None) {
                            eprintln!("[ios-rsd] client error: {e:#}");
                        }
                    })
                    .ok();
            }
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct TunnelEntry {
    udid: String,
    product_type: String,
    os_version: String,
    device_name: String,
    ecid: u64,
    services: u64,
}

/// Ensure the daemon is running (spawns it if not).
pub fn start(listen: Option<String>, output: OutputMode) -> Result<()> {
    let addr = resolve_listen_addr(listen.as_deref())?;
    let addr_str = listen_addr_display(&addr);

    if probe_daemon(&addr) {
        if output.is_json() {
            return print_json(&ActionResult::with_msg("daemon already running"));
        }
        println!("daemon already running  {addr_str}");
        return Ok(());
    }

    let exe = std::env::current_exe().context("current_exe")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("tunnel");
    if let Some(ref l) = listen {
        cmd.args(["--listen", l]);
    } else if let Ok(env_val) = std::env::var(ios_rs::tunnel::DAEMON_SOCKET_ENV) {
        cmd.env(ios_rs::tunnel::DAEMON_SOCKET_ENV, env_val);
    }
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("spawn daemon")?;

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if probe_daemon(&addr) {
            if output.is_json() {
                return print_json(&ActionResult::with_msg("daemon started"));
            }
            println!("daemon started  {addr_str}");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("daemon did not start within 15 s");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn listen_addr_display(addr: &ListenAddr) -> String {
    match addr {
        ListenAddr::Unix(p) => format!("socket={p}"),
        ListenAddr::Tcp(a) => format!("tcp={a}"),
    }
}

/// Send a Shutdown request and wait for the daemon to exit.
pub fn stop(listen: Option<String>, output: OutputMode) -> Result<()> {
    let addr = resolve_listen_addr(listen.as_deref())?;
    let mut s = connect_daemon(&addr)?;
    let mut d = plist::Dictionary::new();
    d.insert("Request".into(), plist::Value::String("Shutdown".into()));
    send_plist(&mut s, &plist::Value::Dictionary(d))?;
    if output.is_json() {
        print_json(&ActionResult::with_msg("daemon stopped"))?;
    } else {
        println!("daemon stopped");
    }
    Ok(())
}

/// List active tunnels.
pub fn list(listen: Option<String>, output: OutputMode) -> Result<()> {
    let addr = resolve_listen_addr(listen.as_deref())?;
    let mut s =
        connect_daemon(&addr).context("daemon not running (start with: ios tunnel start)")?;
    let mut d = plist::Dictionary::new();
    d.insert("Request".into(), plist::Value::String("List".into()));
    send_plist(&mut s, &plist::Value::Dictionary(d))?;

    let resp = recv_plist(&mut s)?;
    let dict = resp.as_dictionary().context("bad response")?;
    if dict.get("Status").and_then(|v| v.as_string()) != Some("Ok") {
        let err = dict
            .get("Error")
            .and_then(|v| v.as_string())
            .unwrap_or("unknown");
        bail!("daemon error: {err}");
    }

    let devices = dict
        .get("Devices")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    if output.is_json() {
        let entries: Vec<TunnelEntry> = devices
            .iter()
            .filter_map(|entry| {
                let dd = entry.as_dictionary()?;
                let str_val = |k| {
                    dd.get(k)
                        .and_then(|v| v.as_string())
                        .unwrap_or("")
                        .to_owned()
                };
                Some(TunnelEntry {
                    udid: str_val("UDID"),
                    product_type: str_val("ProductType"),
                    os_version: str_val("OSVersion"),
                    device_name: str_val("DeviceName"),
                    ecid: dd
                        .get("ECID")
                        .and_then(|v| v.as_unsigned_integer())
                        .unwrap_or(0),
                    services: dd
                        .get("Services")
                        .and_then(|v| v.as_unsigned_integer())
                        .unwrap_or(0),
                })
            })
            .collect();
        return print_json(&entries);
    }

    if devices.is_empty() {
        println!("no active tunnels");
        return Ok(());
    }
    for entry in devices {
        if let Some(dd) = entry.as_dictionary() {
            let udid = dd.get("UDID").and_then(|v| v.as_string()).unwrap_or("?");
            let name = dd
                .get("DeviceName")
                .and_then(|v| v.as_string())
                .unwrap_or("?");
            let product = dd
                .get("ProductType")
                .and_then(|v| v.as_string())
                .unwrap_or("?");
            let os = dd
                .get("OSVersion")
                .and_then(|v| v.as_string())
                .unwrap_or("?");
            let n = dd
                .get("Services")
                .and_then(|v| v.as_unsigned_integer())
                .unwrap_or(0);
            println!("{udid}  {name}  {product}  iOS {os}  ({n} services)");
        }
    }
    Ok(())
}

// ── daemon internals ──────────────────────────────────────────────────────────

fn watch_devices(state: State) {
    let conn = match MuxConn::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ios-rsd] usbmux open: {e}");
            return;
        }
    };
    let mut listener = match conn.listen() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[ios-rsd] usbmux listen: {e}");
            return;
        }
    };
    loop {
        match listener.next() {
            Ok(Event::DeviceAttached(dev)) => {
                let udid = dev.serial.clone();
                let s = Arc::clone(&state);
                std::thread::Builder::new()
                    .name(format!("tunnel-{udid}"))
                    .spawn(move || {
                        if let Err(e) = ensure_tunnel(&udid, &s) {
                            eprintln!("[ios-rsd] tunnel failed for {udid}: {e:#}");
                        }
                    })
                    .ok();
            }
            Ok(Event::DeviceList(devices)) => {
                for dev in devices {
                    let udid = dev.serial.clone();
                    let s = Arc::clone(&state);
                    std::thread::Builder::new()
                        .name(format!("tunnel-{udid}"))
                        .spawn(move || {
                            if let Err(e) = ensure_tunnel(&udid, &s) {
                                eprintln!("[ios-rsd] tunnel failed for {udid}: {e:#}");
                            }
                        })
                        .ok();
                }
            }
            Ok(Event::DeviceDetached { device_id }) => {
                let mut s = state.lock().unwrap();
                let mut detached = Vec::new();
                s.tunnels.retain(|udid, dt| {
                    if dt.device_id == device_id {
                        eprintln!("[ios-rsd] {udid} disconnected — tunnel removed");
                        detached.push(udid.clone());
                        false
                    } else {
                        true
                    }
                });
                for udid in detached {
                    broadcast_locked(&mut s, WatchEvent::Detached { udid });
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[ios-rsd] usbmux event error: {e}");
                break;
            }
        }
    }
}

fn handle_client(mut stream: MuxSocket, state: &State, socket_path: Option<&str>) -> Result<()> {
    // A failed initial read usually means a liveness probe (connected + immediately dropped).
    let req = match recv_plist(&mut stream) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let dict = req.as_dictionary().context("request not a dict")?;

    // Control requests (no UDID/Service keys)
    if let Some(rt) = dict.get("Request").and_then(|v| v.as_string()) {
        match rt {
            "List" => return handle_list(&mut stream, state),
            "Watch" => return handle_watch(stream, state),
            "Shutdown" => {
                send_ok(&mut stream).ok();
                eprintln!("[ios-rsd] shutdown requested");
                if let Some(path) = socket_path {
                    let _ = std::fs::remove_file(path);
                }
                let _ = std::fs::remove_file(PID_PATH);
                std::process::exit(0);
            }
            _ => {}
        }
    }

    let udid = dict
        .get("UDID")
        .and_then(|v| v.as_string())
        .context("no UDID")?
        .to_owned();
    let service = dict
        .get("Service")
        .and_then(|v| v.as_string())
        .context("no Service")?
        .to_owned();

    if let Err(e) = ensure_tunnel(&udid, state) {
        return send_err(&mut stream, &format!("{e:#}"));
    }

    // "_rsd" — raw proxy to the RSD port; RemoteXPC + handshake done by the client.
    if service == "_rsd" {
        let (tunnel, server_addr, rsd_port) = {
            let s = state.lock().unwrap();
            let dt = s.tunnels.get(&udid).context("tunnel vanished")?;
            (
                Arc::clone(&dt.tunnel),
                dt.tunnel.params.server_addr,
                dt.tunnel.params.server_rsd_port,
            )
        };
        match tunnel.connect(server_addr, rsd_port) {
            Ok(smc) => {
                send_ok(&mut stream)?;
                proxy(stream, smc);
            }
            Err(e) => send_err(&mut stream, &e.to_string())?,
        }
        return Ok(());
    }

    // Regular service — look up port in RSD catalog.
    let port = {
        let s = state.lock().unwrap();
        let dt = s.tunnels.get(&udid).context("tunnel vanished")?;
        match dt.services.get(&service).map(|e| e.port) {
            Some(p) => p,
            None => {
                drop(s);
                return send_err(
                    &mut stream,
                    &format!("service '{service}' not in RSD catalog"),
                );
            }
        }
    };

    // Clone Arc so connect() doesn't hold the mutex.
    let (tunnel, server_addr) = {
        let s = state.lock().unwrap();
        let dt = s.tunnels.get(&udid).context("tunnel vanished")?;
        (Arc::clone(&dt.tunnel), dt.tunnel.params.server_addr)
    };

    let smc = match tunnel.connect(server_addr, port) {
        Ok(s) => s,
        Err(e) => return send_err(&mut stream, &e.to_string()),
    };

    // Shim services need an RSDCheckin before the real protocol starts.
    if service.ends_with(".shim.remote") {
        if let Err(e) = rsd_checkin(&smc) {
            return send_err(&mut stream, &format!("RSDCheckin: {e}"));
        }
    }

    send_ok(&mut stream)?;
    proxy(stream, smc);
    Ok(())
}

fn handle_list(stream: &mut MuxSocket, state: &State) -> Result<()> {
    let devices: Vec<plist::Value> = {
        let s = state.lock().unwrap();
        s.tunnels
            .iter()
            .map(|(udid, dt)| plist::Value::Dictionary(tunnel_plist_dict(udid, dt)))
            .collect()
    };
    let mut resp = plist::Dictionary::new();
    resp.insert("Status".into(), plist::Value::String("Ok".into()));
    resp.insert("Devices".into(), plist::Value::Array(devices));
    send_plist(stream, &plist::Value::Dictionary(resp))
}

fn handle_watch(mut stream: MuxSocket, state: &State) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    {
        let mut s = state.lock().unwrap();
        // Bootstrap: send all currently active tunnels as Attached events.
        for (udid, dt) in &s.tunnels {
            let dict = tunnel_plist_dict(udid, dt);
            tx.send(WatchEvent::Attached(dict)).ok();
        }
        s.watchers.push(tx);
    }
    for event in rx {
        let val = watch_event_to_plist(event);
        if send_plist(&mut stream, &val).is_err() {
            break;
        }
    }
    Ok(())
}

fn watch_event_to_plist(event: WatchEvent) -> plist::Value {
    match event {
        WatchEvent::Attached(mut dict) => {
            dict.insert("Event".into(), plist::Value::String("Attached".into()));
            plist::Value::Dictionary(dict)
        }
        WatchEvent::Detached { udid } => {
            let mut d = plist::Dictionary::new();
            d.insert("Event".into(), plist::Value::String("Detached".into()));
            d.insert("UDID".into(), plist::Value::String(udid));
            plist::Value::Dictionary(d)
        }
    }
}

/// Establish (or reuse) a CDTunnel + smoltcp stack for `udid`.
fn ensure_tunnel(udid: &str, state: &State) -> Result<()> {
    if state.lock().unwrap().tunnels.contains_key(udid) {
        return Ok(());
    }
    eprintln!("[ios-rsd] establishing tunnel for {udid}…");

    let mut conn = MuxConn::open().context("usbmux open")?;
    let devices = conn.list_devices().context("list devices")?;
    let device = devices
        .iter()
        .find(|d| d.serial.eq_ignore_ascii_case(udid))
        .with_context(|| format!("device {udid} not connected"))?;
    let device_id = device.device_id;

    // Fetch device_name and ECID from lockdown before establishing CDTunnel.
    let (device_name, ecid) = {
        let mut ld = LockdownSession::open_paired(device_id, udid)
            .context("lockdown open for device metadata")?;
        let info = ld.get_all_values().context("lockdown get_all_values")?;
        let ecid = ld
            .get_value(None, "UniqueChipID")
            .unwrap_or(plist::Value::Integer(0.into()))
            .as_unsigned_integer()
            .unwrap_or(0);
        (info.device_name, ecid)
    };

    let version = detect_version(device_id).context("detect version")?;
    let t = Ios17Tunnel::connect_via_lockdown_udid(device_id, Some(udid), version)
        .context("CDTunnel")?;
    let rsd = t.connect_rsd().context("RSD handshake")?;
    let peer_info = rsd.peer_info().clone();
    let services = rsd.services().clone();
    let n = services.len();

    {
        let mut s = state.lock().unwrap();
        // Use Entry to detect whether we actually insert (two threads may race here).
        let newly_inserted = match s.tunnels.entry(udid.to_owned()) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(DeviceTunnel {
                    tunnel: Arc::new(t.stack),
                    services,
                    device_id,
                    peer_info,
                    device_name,
                    ecid,
                });
                true
            }
        };
        if newly_inserted {
            let dict = tunnel_plist_dict(udid, s.tunnels.get(udid).unwrap());
            broadcast_locked(&mut s, WatchEvent::Attached(dict));
        }
    }
    eprintln!("[ios-rsd] tunnel ready for {udid} ({n} services)");
    Ok(())
}

fn rsd_checkin(stream: &std::os::unix::net::UnixStream) -> Result<()> {
    let mut s = stream.try_clone()?;
    let plist_bytes = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"",
        " \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\"><dict>",
        "<key>Label</key><string>devicelink</string>",
        "<key>ProtocolVersion</key><string>2</string>",
        "<key>Request</key><string>RSDCheckin</string>",
        "</dict></plist>\n",
    )
    .as_bytes();
    let len = plist_bytes.len() as u32;
    s.write_all(&len.to_be_bytes())?;
    s.write_all(plist_bytes)?;
    s.flush()?;

    for _ in 0..4 {
        let mut len_buf = [0u8; 4];
        read_exact(&mut s, &mut len_buf)?;
        let n = u32::from_be_bytes(len_buf) as usize;
        let mut body = vec![0u8; n];
        read_exact(&mut s, &mut body)?;
        if let Ok(plist::Value::Dictionary(d)) = plist::from_bytes::<plist::Value>(&body) {
            if d.get("Request").and_then(|v| v.as_string()) == Some("StartService") {
                break;
            }
        }
    }
    Ok(())
}

/// Copy bytes between the IPC socket and the smoltcp UnixStream pipe.
fn proxy(ipc: MuxSocket, smc: std::os::unix::net::UnixStream) {
    let mut ipc_r = match ipc.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut smc_r = match smc.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut ipc_w = ipc;
    let mut smc_w = smc;
    std::thread::spawn(move || {
        io::copy(&mut smc_r, &mut ipc_w).ok();
    });
    io::copy(&mut ipc_r, &mut smc_w).ok();
}

// ── plist framing (4-byte BE length prefix + XML plist) ───────────────────────

fn send_plist(s: &mut impl Write, val: &plist::Value) -> Result<()> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, val)?;
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()?;
    Ok(())
}

fn recv_plist(s: &mut impl Read) -> Result<plist::Value> {
    let mut len_buf = [0u8; 4];
    read_exact(s, &mut len_buf)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(n > 0 && n < 1_048_576, "bad plist length {n}");
    let mut body = vec![0u8; n];
    read_exact(s, &mut body)?;
    Ok(plist::from_bytes(&body)?)
}

fn send_ok(s: &mut impl Write) -> Result<()> {
    let mut d = plist::Dictionary::new();
    d.insert("Status".into(), plist::Value::String("Ok".into()));
    send_plist(s, &plist::Value::Dictionary(d))
}

fn send_err(s: &mut impl Write, msg: &str) -> Result<()> {
    let mut d = plist::Dictionary::new();
    d.insert("Status".into(), plist::Value::String("Error".into()));
    d.insert("Error".into(), plist::Value::String(msg.into()));
    send_plist(s, &plist::Value::Dictionary(d))
}

fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read(&mut buf[done..])?;
        anyhow::ensure!(n > 0, "unexpected EOF");
        done += n;
    }
    Ok(())
}
