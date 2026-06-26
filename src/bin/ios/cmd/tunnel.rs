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
use ios_rs::tunnel::{detect_version, Ios17Tunnel, RawCdStream, SmoltcpTunnel, DAEMON_SOCKET};
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
    /// The device's own RemotePairing identity UUID (from SRP M6).
    /// This is what the device would advertise in _remotepairing._tcp mDNS.
    peer_pairing_identifier: String,
    /// Cached RSD Handshake message, replayed to answer `_rsd` connections
    /// instantly from cache (so CoreDevice sees ProductType without a round-trip).
    #[allow(dead_code)] // disabled tunnelservice probe; kept for reference
    rsd_handshake: ios_rs::xpc::Message,
    /// Canonical device values from lockdown `GetValue(nil,nil)`, fetched over
    /// usbmux→lockdownd BEFORE the CDTunnel/RSD handshake.  This is the authoritative,
    /// always-available source for ProductType / BuildVersion / ProductVersion /
    /// HardwareModel / CPUArchitecture that CoreDeviceService needs to derive the
    /// device's supported features (platform/deviceType/osBuildUpdate) — without which
    /// the tunnel usage assertion fails RemotePairingError 1005.
    lockdown_values: plist::Dictionary,
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
    // OS build (e.g. "23B85") → CoreDeviceService's osBuildUpdate.  Prefer the canonical
    // lockdown value (fetched pre-handshake); fall back to the RSD handshake's value.
    let build_version = dt
        .lockdown_values
        .get("BuildVersion")
        .and_then(|v| v.as_string())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| dt.peer_info.build_version.clone());
    d.insert("BuildVersion".into(), plist::Value::String(build_version));
    // Canonical, pre-handshake device values from lockdown GetValue(nil,nil)
    // (ProductType/ProductVersion/BuildVersion/HardwareModel/CPUArchitecture/…).  This is
    // the authoritative set the bridge serves to CoreDevice so it can derive supported
    // features (platform/deviceType/osBuildUpdate) and not fail the tunnel assertion 1005.
    // (The device's full RSD Properties are already cached in DeviceTunnel.rsd_handshake.)
    d.insert(
        "LockdownValues".into(),
        plist::Value::Dictionary(dt.lockdown_values.clone()),
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
    // Full service catalog: name → port, so the client can open sockets for all services.
    let mut svc_ports = plist::Dictionary::new();
    for (name, entry) in &dt.services {
        svc_ports.insert(
            name.clone(),
            plist::Value::Integer(plist::Integer::from(entry.port as i64)),
        );
    }
    d.insert("ServicePorts".into(), plist::Value::Dictionary(svc_ports));
    // The device's own RemotePairing identity UUID — what it uses in _remotepairing._tcp mDNS.
    if !dt.peer_pairing_identifier.is_empty() {
        d.insert(
            "RemotePairingIdentifier".into(),
            plist::Value::String(dt.peer_pairing_identifier.clone()),
        );
    }

    // CDTunnel parameters so clients can construct the IPv6 layer themselves.
    d.insert(
        "TunnelServerAddr".into(),
        plist::Value::String(dt.tunnel.params.server_addr.to_string()),
    );
    d.insert(
        "TunnelClientAddr".into(),
        plist::Value::String(dt.tunnel.params.client_addr.to_string()),
    );
    d.insert(
        "TunnelRSDPort".into(),
        plist::Value::Integer(plist::Integer::from(
            dt.tunnel.params.server_rsd_port as i64,
        )),
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
            "RawTunnel" => {
                let udid = dict
                    .get("UDID")
                    .and_then(|v| v.as_string())
                    .context("RawTunnel: no UDID")?
                    .to_owned();
                return handle_raw_tunnel(stream, &udid);
            }
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

    // Raw port proxies (RemoteXPC + handshake done by the client):
    //   "_rsd"          — raw proxy to the device's RSD port.
    //   "_rawport:<N>"  — raw proxy to an arbitrary device port <N>.  Used by the
    //                     bridge for the DYNAMIC ports that the device's
    //                     untrusted.tunnelservice spawns via get_service (coredevice
    //                     deviceinfo/appservice/etc.), which are not in the RSD
    //                     catalog and therefore have no named service.
    let rawport: Option<u16> = if service == "_rsd" {
        Some(0) // sentinel: use server_rsd_port
    } else {
        service
            .strip_prefix("_rawport:")
            .and_then(|p| p.parse::<u16>().ok())
    };
    if let Some(req_port) = rawport {
        let (tunnel, server_addr, port) = {
            let s = state.lock().unwrap();
            let dt = s.tunnels.get(&udid).context("tunnel vanished")?;
            let port = if req_port == 0 {
                dt.tunnel.params.server_rsd_port
            } else {
                req_port
            };
            (Arc::clone(&dt.tunnel), dt.tunnel.params.server_addr, port)
        };
        match tunnel.connect(server_addr, port) {
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

    // Fetch device_name, ECID and the canonical device values from lockdown BEFORE
    // establishing the CDTunnel — this is the authoritative, pre-handshake source for
    // ProductType/BuildVersion/etc. that CoreDeviceService needs (see DeviceTunnel).
    let (device_name, ecid, lockdown_values) = {
        let mut ld = LockdownSession::open_paired(device_id, udid)
            .context("lockdown open for device metadata")?;
        let info = ld.get_all_values().context("lockdown get_all_values")?;
        let ecid = ld
            .get_value(None, "UniqueChipID")
            .unwrap_or(plist::Value::Integer(0.into()))
            .as_unsigned_integer()
            .unwrap_or(0);
        let mut lv = plist::Dictionary::new();
        let mut put = |k: &str, s: &str| {
            if !s.is_empty() {
                lv.insert(k.into(), plist::Value::String(s.to_owned()));
            }
        };
        put("ProductType", &info.product_type);
        put("ProductVersion", &info.product_version);
        put("HardwareModel", &info.hardware_model);
        put("CPUArchitecture", &info.cpu_architecture);
        put("UniqueDeviceID", &info.unique_device_id);
        put("SerialNumber", &info.serial_number);
        // BuildVersion (e.g. "23B85") + any other useful keys live in `extra`.
        for k in [
            "BuildVersion",
            "ProductName",
            "DeviceClass",
            "ChipID",
            "HardwarePlatform",
        ] {
            if let Some(v) = info.extra.get(k) {
                lv.insert(k.into(), v.clone());
            }
        }
        eprintln!(
            "[ios-rsd] lockdown values for {udid}: {:?}",
            lv.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
        );
        (info.device_name, ecid, lv)
    };

    let version = detect_version(device_id).context("detect version")?;
    let t = Ios17Tunnel::connect_via_lockdown_udid(device_id, Some(udid), version)
        .context("CDTunnel")?;
    let rsd = t.connect_rsd().context("RSD handshake")?;
    let peer_info = rsd.peer_info().clone();
    let services = rsd.services().clone();
    let rsd_handshake = rsd.handshake().clone();
    let n = services.len();

    let tunnel_arc = Arc::new(t.stack);
    {
        let mut s = state.lock().unwrap();
        // Use Entry to detect whether we actually insert (two threads may race here).
        let newly_inserted = match s.tunnels.entry(udid.to_owned()) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(DeviceTunnel {
                    tunnel: Arc::clone(&tunnel_arc),
                    services: services.clone(),
                    device_id,
                    peer_info,
                    device_name,
                    ecid,
                    peer_pairing_identifier: String::new(),
                    rsd_handshake,
                    lockdown_values,
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

    // NOTE: a background probe to com.apple.internal.dt.coredevice.untrusted.tunnelservice
    // used to run here to fetch the CoreDevice identifier.  It never worked (timed out) and
    // — critically — opening that service appears to disturb CoreDevice's own use of it,
    // leaving CoreDeviceService unable to derive device features (platform/deviceType/osBuild
    // come back nil, so the device is stuck "Connecting").  Disabled.
    let _ = &tunnel_arc;
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

/// Handle a `{Request:"RawTunnel", UDID}` request: open a fresh CoreDeviceProxy CDTunnel
/// (no smoltcp), reply with the negotiated tunnel params, then splice the raw IPv6 stream
/// straight to the client. Lets a caller (the device bridge) run a transparent relay.
fn handle_raw_tunnel(mut stream: MuxSocket, udid: &str) -> Result<()> {
    let device_id = {
        let mut conn = MuxConn::open().context("usbmux open")?;
        let devices = conn.list_devices().context("list devices")?;
        match devices.iter().find(|d| d.serial.eq_ignore_ascii_case(udid)) {
            Some(d) => d.device_id,
            None => return send_err(&mut stream, &format!("device {udid} not connected")),
        }
    };
    let (dev, params) = match Ios17Tunnel::connect_raw_cdtunnel(device_id, udid) {
        Ok(x) => x,
        Err(e) => return send_err(&mut stream, &format!("raw CDTunnel: {e:#}")),
    };
    eprintln!(
        "[ios-rsd] RAW tunnel for {udid}: client={} server={} rsd={} mtu={}",
        params.client_addr, params.server_addr, params.server_rsd_port, params.mtu
    );
    let mut resp = plist::Dictionary::new();
    resp.insert("Status".into(), plist::Value::String("Ok".into()));
    resp.insert(
        "ServerAddr".into(),
        plist::Value::String(params.server_addr.to_string()),
    );
    resp.insert(
        "ClientAddr".into(),
        plist::Value::String(params.client_addr.to_string()),
    );
    resp.insert(
        "RSDPort".into(),
        plist::Value::Integer((params.server_rsd_port as u64).into()),
    );
    resp.insert(
        "MTU".into(),
        plist::Value::Integer((params.mtu as u64).into()),
    );
    send_plist(&mut stream, &plist::Value::Dictionary(resp))?;
    splice_raw(dev, stream);
    Ok(())
}

/// Splice raw IPv6 bytes between a raw CoreDeviceProxy stream (TLS — NOT splittable for
/// concurrent read+write) and the daemon client socket. One thread owns `dev` (polls it
/// with a short read timeout, and drains a channel of client→device bytes); a second
/// thread blocking-reads the client into that channel.
fn splice_raw(mut dev: RawCdStream, client: MuxSocket) {
    let mut client_w = match client.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut client_r = client;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match client_r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    dev.tcp()
        .set_read_timeout(Some(Duration::from_millis(2)))
        .ok();
    let mut buf = vec![0u8; 65536];
    loop {
        match dev.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if client_w.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = client_w.flush();
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(_) => break,
        }
        loop {
            match rx.try_recv() {
                Ok(data) => {
                    if dev.write_all(&data).is_err() {
                        return;
                    }
                    let _ = dev.flush();
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
    }
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

/// Query the CoreDevice identifier from `com.apple.internal.dt.coredevice.untrusted.tunnelservice`.
///
/// Runs the actual RPC in an inner thread and enforces a 10-second timeout via channel,
/// because the smoltcp UnixStream socket pair does not honour SO_RCVTIMEO and the service
/// may never respond if the protocol framing is wrong.
#[allow(dead_code)] // disabled tunnelservice probe (superseded by raw relay)
fn get_coredevice_identifier(
    tunnel: Arc<SmoltcpTunnel>,
    port: Option<u16>,
    udid: &str,
) -> Option<String> {
    eprintln!("[ios-rsd] get_coredevice_identifier: udid={udid} port={port:?}");
    let port = port.or_else(|| {
        eprintln!("[ios-rsd] untrusted.tunnelservice not in catalog");
        None
    })?;
    let udid = udid.to_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("coredevice-rpc-{udid}"))
        .spawn(move || {
            tx.send(query_tunnelservice_identifier(&tunnel, port, &udid))
                .ok();
        })
        .ok();
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(_) => {
            eprintln!("[ios-rsd] coredevice identifier query timed out");
            None
        }
    }
}

/// Inner blocking query — called from a dedicated thread.
#[allow(dead_code)]
fn query_tunnelservice_identifier(stack: &SmoltcpTunnel, port: u16, _udid: &str) -> Option<String> {
    use ios_rs::xpc::Value;
    use std::collections::HashMap;
    use std::net::TcpListener;

    let stream = stack
        .connect(stack.params.server_addr, port)
        .map_err(|e| eprintln!("[ios-rsd] untrusted.tunnelservice connect: {e}"))
        .ok()?;

    // Relay the UnixStream through a loopback TCP socket (RemoteXpcConn needs TcpStream).
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let relay_addr = listener.local_addr().ok()?;
    std::thread::spawn(move || {
        if let Ok((server, _)) = listener.accept() {
            let mut r = stream.try_clone().unwrap();
            let mut w = stream;
            let mut tw = server.try_clone().unwrap();
            let mut tr = server;
            let t1 = std::thread::spawn(move || {
                std::io::copy(&mut r, &mut tw).ok();
            });
            let t2 = std::thread::spawn(move || {
                std::io::copy(&mut tr, &mut w).ok();
            });
            let _ = (t1.join(), t2.join());
        }
    });

    let conn = ios_rs::remotexpc::RemoteXpcConn::connect(relay_addr)
        .map_err(|e| eprintln!("[ios-rsd] untrusted.tunnelservice XPC: {e}"))
        .ok()?;

    // The untrusted.tunnelservice speaks a cmd-based protocol, not the flat CoreDevice.* format.
    // Step 1: list_services to discover available services and their ports.
    let mut list_req: HashMap<String, Value> = HashMap::new();
    list_req.insert("cmd".into(), Value::String("list_services".into()));
    match conn.request(Value::Dictionary(list_req)) {
        Ok(reply) => {
            eprintln!("[ios-rsd] list_services reply: {reply:?}");
        }
        Err(e) => {
            eprintln!("[ios-rsd] list_services failed: {e}");
            return None;
        }
    }

    // Step 2: get_service for deviceinfo
    let mut get_req: HashMap<String, Value> = HashMap::new();
    get_req.insert("cmd".into(), Value::String("get_service".into()));
    get_req.insert(
        "name".into(),
        Value::String("com.apple.coredevice.deviceinfo".into()),
    );
    match conn.request(Value::Dictionary(get_req)) {
        Ok(reply) => {
            eprintln!("[ios-rsd] get_service deviceinfo reply: {reply:?}");
            extract_identifier_from_reply(&reply)
        }
        Err(e) => {
            eprintln!("[ios-rsd] get_service failed: {e}");
            None
        }
    }
}

#[allow(dead_code)]
fn extract_identifier_from_reply(reply: &ios_rs::xpc::Value) -> Option<String> {
    use ios_rs::xpc::Value;
    let dict = reply.as_dict()?;
    let Some(output_val) = dict.get("CoreDevice.output") else {
        eprintln!(
            "[ios-rsd] getdeviceinfo reply keys: {:?}",
            dict.keys().collect::<Vec<_>>()
        );
        return None;
    };
    let output = output_val.as_dict()?;

    // Try common paths for the device UUID in getdeviceinfo response.
    for key in &["deviceIdentifier", "identifier", "uniqueDeviceIdentifier"] {
        if let Some(Value::String(s)) = output.get(*key) {
            if !s.is_empty() {
                eprintln!("[ios-rsd] getdeviceinfo.{key} = {s}");
                return Some(s.clone());
            }
        }
    }

    // Log all string keys in the output for debugging.
    eprintln!(
        "[ios-rsd] getdeviceinfo output keys: {:?}",
        output.keys().collect::<Vec<_>>()
    );
    None
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
