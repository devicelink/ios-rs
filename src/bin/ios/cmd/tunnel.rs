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
///     {Request: "List"}     → {Status: "Ok", Devices: [{UDID, Services}…]}
///     {Request: "Shutdown"} → daemon exits
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::cmd::output::{print_json, ActionResult, OutputMode};
use anyhow::{bail, Context, Result};
use ios_rs::rsd::ServiceEntry;
use ios_rs::tunnel::{detect_version, Ios17Tunnel, SmoltcpTunnel, DAEMON_SOCKET};
use ios_rs::usbmux::{Connection as MuxConn, Event};

const PID_PATH: &str = "/tmp/ios-rsd.pid";

// ── shared state ──────────────────────────────────────────────────────────────

struct DeviceTunnel {
    tunnel: Arc<SmoltcpTunnel>,
    services: HashMap<String, ServiceEntry>,
    device_id: u32,
}

type State = Arc<Mutex<HashMap<String, DeviceTunnel>>>;

// ── public CLI entry-points ───────────────────────────────────────────────────

/// Run the daemon process (blocks until told to shut down).
pub fn daemon() -> Result<()> {
    let _ = std::fs::write(PID_PATH, std::process::id().to_string());
    let _ = std::fs::remove_file(DAEMON_SOCKET);

    let listener =
        UnixListener::bind(DAEMON_SOCKET).with_context(|| format!("bind {DAEMON_SOCKET}"))?;
    eprintln!(
        "[ios-rsd] started  pid={}  socket={DAEMON_SOCKET}",
        std::process::id()
    );

    let state: State = Arc::new(Mutex::new(HashMap::new()));

    // Background thread: watch usbmuxd for device disconnects.
    let watcher_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("usbmux-watcher".into())
        .spawn(move || watch_devices(watcher_state))
        .context("spawn watcher thread")?;

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("ipc-client".into())
            .spawn(move || {
                if let Err(e) = handle_client(stream, &state) {
                    eprintln!("[ios-rsd] client error: {e:#}");
                }
            })
            .ok();
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct TunnelEntry {
    udid: String,
    services: u64,
}

/// Ensure the daemon is running (spawns it if not).
pub fn start(output: OutputMode) -> Result<()> {
    if UnixStream::connect(DAEMON_SOCKET).is_ok() {
        if output.is_json() {
            return print_json(&ActionResult::with_msg("daemon already running"));
        }
        println!("daemon already running  socket={DAEMON_SOCKET}");
        return Ok(());
    }
    let exe = std::env::current_exe().context("current_exe")?;
    std::process::Command::new(&exe)
        .args(["tunnel", "daemon"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("spawn daemon")?;

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if UnixStream::connect(DAEMON_SOCKET).is_ok() {
            if output.is_json() {
                return print_json(&ActionResult::with_msg("daemon started"));
            }
            println!("daemon started  socket={DAEMON_SOCKET}");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("daemon did not start within 15 s");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Send a Shutdown request and wait for the daemon to exit.
pub fn stop(output: OutputMode) -> Result<()> {
    let mut s = UnixStream::connect(DAEMON_SOCKET).context("daemon not running")?;
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
pub fn list(output: OutputMode) -> Result<()> {
    let mut s = UnixStream::connect(DAEMON_SOCKET)
        .context("daemon not running (start with: ios tunnel start)")?;
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
                let udid = dd
                    .get("UDID")
                    .and_then(|v| v.as_string())
                    .unwrap_or("?")
                    .to_owned();
                let services = dd
                    .get("Services")
                    .and_then(|v| v.as_unsigned_integer())
                    .unwrap_or(0);
                Some(TunnelEntry { udid, services })
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
            let n = dd
                .get("Services")
                .and_then(|v| v.as_unsigned_integer())
                .unwrap_or(0);
            println!("{udid}  ({n} services)");
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
            Ok(Event::DeviceDetached { device_id }) => {
                let mut map = state.lock().unwrap();
                map.retain(|udid, dt| {
                    if dt.device_id == device_id {
                        eprintln!("[ios-rsd] {udid} disconnected — tunnel removed");
                        false
                    } else {
                        true
                    }
                });
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[ios-rsd] usbmux event error: {e}");
                break;
            }
        }
    }
}

fn handle_client(mut stream: UnixStream, state: &State) -> Result<()> {
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
            "Shutdown" => {
                send_ok(&mut stream).ok();
                eprintln!("[ios-rsd] shutdown requested");
                let _ = std::fs::remove_file(DAEMON_SOCKET);
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
            let map = state.lock().unwrap();
            let dt = map.get(&udid).context("tunnel vanished")?;
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
        let map = state.lock().unwrap();
        let dt = map.get(&udid).context("tunnel vanished")?;
        match dt.services.get(&service).map(|e| e.port) {
            Some(p) => p,
            None => {
                drop(map);
                return send_err(
                    &mut stream,
                    &format!("service '{service}' not in RSD catalog"),
                );
            }
        }
    };

    // Clone Arc so connect() doesn't hold the mutex.
    let (tunnel, server_addr) = {
        let map = state.lock().unwrap();
        let dt = map.get(&udid).context("tunnel vanished")?;
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

fn handle_list(stream: &mut UnixStream, state: &State) -> Result<()> {
    let devices: Vec<plist::Value> = {
        let map = state.lock().unwrap();
        map.iter()
            .map(|(udid, dt)| {
                let mut d = plist::Dictionary::new();
                d.insert("UDID".into(), plist::Value::String(udid.clone()));
                d.insert(
                    "Services".into(),
                    plist::Value::Integer(plist::Integer::from(dt.services.len() as i64)),
                );
                plist::Value::Dictionary(d)
            })
            .collect()
    };
    let mut resp = plist::Dictionary::new();
    resp.insert("Status".into(), plist::Value::String("Ok".into()));
    resp.insert("Devices".into(), plist::Value::Array(devices));
    send_plist(stream, &plist::Value::Dictionary(resp))
}

/// Establish (or reuse) a CDTunnel + smoltcp stack for `udid`.
fn ensure_tunnel(udid: &str, state: &State) -> Result<()> {
    if state.lock().unwrap().contains_key(udid) {
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

    let version = detect_version(device_id).context("detect version")?;
    let t = Ios17Tunnel::connect_via_lockdown_udid(device_id, Some(udid), version)
        .context("CDTunnel")?;
    let rsd = t.connect_rsd().context("RSD handshake")?;
    let services = rsd.services().clone();
    let n = services.len();

    {
        let mut map = state.lock().unwrap();
        // or_insert: safe to lose the race — the winner's tunnel is used.
        map.entry(udid.to_owned()).or_insert(DeviceTunnel {
            tunnel: Arc::new(t.stack),
            services,
            device_id,
        });
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
fn proxy(ipc: UnixStream, smc: std::os::unix::net::UnixStream) {
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

fn send_plist(s: &mut UnixStream, val: &plist::Value) -> Result<()> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, val)?;
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()?;
    Ok(())
}

fn recv_plist(s: &mut UnixStream) -> Result<plist::Value> {
    let mut len_buf = [0u8; 4];
    read_exact(s, &mut len_buf)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(n > 0 && n < 1_048_576, "bad plist length {n}");
    let mut body = vec![0u8; n];
    read_exact(s, &mut body)?;
    Ok(plist::from_bytes(&body)?)
}

fn send_ok(s: &mut UnixStream) -> Result<()> {
    let mut d = plist::Dictionary::new();
    d.insert("Status".into(), plist::Value::String("Ok".into()));
    send_plist(s, &plist::Value::Dictionary(d))
}

fn send_err(s: &mut UnixStream, msg: &str) -> Result<()> {
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
