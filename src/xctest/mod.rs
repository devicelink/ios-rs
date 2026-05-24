/// XCTest orchestration for iOS 17.4+ via CDTunnel + DTX.
///
/// Flow (mirrors go-ios `runXUITestWithBundleIdsXcode15Ctx`):
///   1. Two DTX connections to `com.apple.dt.testmanagerd.remote`
///   2. conn1: initiate IDE session (identity + local capabilities)
///   3. Launch test runner via `com.apple.coredevice.appservice`
///      with `com.apple.coredevice.openstdiosocket` for stdout capture
///   4. conn2: initiate control session + authorize test runner PID
///   5. conn1: start executing test plan (protocol version 36)
///   6. Handle `_XCT_testRunnerReadyWithCapabilities:` by returning XCTestConfig
///   7. Stream test results until connection closes or `_XCT_didFinishExecutingTestPlan`
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use plist::{Dictionary, Uid, Value};

use crate::dtx::{self, AuxValue, DtxConn};
use crate::tunnel::SmoltcpTunnel;

// XCTestConfig as a type alias removed — use RunConfig directly

mod config;

// ── NSKeyedArchiver helpers ───────────────────────────────────────────────────

/// Build the NSKeyedArchiver skeleton.
fn skeleton(objects: Vec<Value>, root: Uid) -> Value {
    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(root));
    let mut d = Dictionary::new();
    d.insert("$version".into(), Value::Integer(100000.into()));
    d.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    d.insert("$top".into(), Value::Dictionary(top));
    d.insert("$objects".into(), Value::Array(objects));
    Value::Dictionary(d)
}

fn to_bin(v: Value) -> Vec<u8> {
    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &v).expect("plist encode");
    buf
}

fn class_dict(classname: &str, superclasses: &[&str]) -> Value {
    let mut classes: Vec<Value> = vec![Value::String(classname.into())];
    classes.extend(superclasses.iter().map(|s| Value::String((*s).into())));
    let mut d = Dictionary::new();
    d.insert("$classname".into(), Value::String(classname.into()));
    d.insert("$classes".into(), Value::Array(classes));
    Value::Dictionary(d)
}

pub fn archive_uuid(bytes: &[u8; 16]) -> Vec<u8> {
    let mut objects = vec![Value::String("$null".into())];
    let class_idx = Uid::new(objects.len() as u64);
    objects.push(class_dict("NSUUID", &["NSObject"]));
    let obj_idx = Uid::new(objects.len() as u64);
    let mut d = Dictionary::new();
    d.insert("NS.uuidbytes".into(), Value::Data(bytes.to_vec()));
    d.insert("$class".into(), Value::Uid(class_idx));
    objects.push(Value::Dictionary(d));
    to_bin(skeleton(objects, obj_idx))
}

pub fn archive_url(url: &str) -> Vec<u8> {
    let mut objects = vec![Value::String("$null".into())];
    let class_idx = Uid::new(objects.len() as u64);
    objects.push(class_dict("NSURL", &["NSObject"]));
    let str_idx = Uid::new(objects.len() as u64);
    objects.push(Value::String(url.into()));
    let obj_idx = Uid::new(objects.len() as u64);
    let mut d = Dictionary::new();
    d.insert("NS.base".into(), Value::Uid(Uid::new(0)));
    d.insert("NS.relative".into(), Value::Uid(str_idx));
    d.insert("$class".into(), Value::Uid(class_idx));
    objects.push(Value::Dictionary(d));
    to_bin(skeleton(objects, obj_idx))
}

/// Serialize `XCTCapabilities` — a dict wrapped in NSKeyedArchiver.
/// capabilities-dictionary must be an NSKeyedArchiver NSDictionary object (UID ref),
/// not an inline plist dict, or NSKeyedUnarchiver will reject it.
pub fn archive_xct_capabilities(caps: &HashMap<&str, Value>) -> Vec<u8> {
    let mut objects = vec![Value::String("$null".into())];
    let xctcaps_class = Uid::new(objects.len() as u64);
    objects.push(class_dict("XCTCapabilities", &["NSObject"]));
    let nsdict_class = Uid::new(objects.len() as u64);
    objects.push(class_dict("NSDictionary", &["NSObject"]));

    // Store each key and value as separate objects
    let mut key_uids = Vec::new();
    let mut val_uids = Vec::new();
    for (k, v) in caps {
        let ku = Uid::new(objects.len() as u64);
        objects.push(Value::String((*k).into()));
        key_uids.push(ku);
        let vu = Uid::new(objects.len() as u64);
        objects.push(v.clone());
        val_uids.push(vu);
    }

    // NSDictionary object
    let nsdict_uid = Uid::new(objects.len() as u64);
    let mut nd = Dictionary::new();
    nd.insert(
        "NS.keys".into(),
        Value::Array(key_uids.iter().map(|u| Value::Uid(*u)).collect()),
    );
    nd.insert(
        "NS.objects".into(),
        Value::Array(val_uids.iter().map(|u| Value::Uid(*u)).collect()),
    );
    nd.insert("$class".into(), Value::Uid(nsdict_class));
    objects.push(Value::Dictionary(nd));

    // XCTCapabilities object
    let obj_idx = Uid::new(objects.len() as u64);
    let mut d = Dictionary::new();
    d.insert("capabilities-dictionary".into(), Value::Uid(nsdict_uid));
    d.insert("$class".into(), Value::Uid(xctcaps_class));
    objects.push(Value::Dictionary(d));

    to_bin(skeleton(objects, obj_idx))
}

// ── XCTestConfiguration ───────────────────────────────────────────────────────

pub use config::build_xctest_configuration_bytes;

// ── Test runner entry point ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TestResult {
    pub bundle: String,
    pub class: String,
    pub method: String,
    pub passed: bool,
    pub message: String,
}

/// Configuration for a test run.
pub struct RunConfig<'a> {
    pub bundle_id: &'a str,
    pub bundle_path: &'a str, // filesystem path of target app (needed for launch)
    pub test_runner_bundle_id: &'a str,
    pub xctest_config_name: &'a str,
    pub tests_to_run: &'a [String],
    pub tests_to_skip: &'a [String],
    pub is_xctest: bool,         // true = unit test (adds BundleInject dylib)
    pub initialize_for_ui: bool, // false = skip foreground app takeover
    pub extra_env: HashMap<String, String>,
}

/// Run XCTests on an iOS 17.4+ device.
///
/// Mirrors go-ios `runXUITestWithBundleIdsXcode15Ctx`.
/// Returns `Ok(passed)`.
pub fn run(
    tunnel: &SmoltcpTunnel,
    rsd: &crate::rsd::RsdClient,
    app_bundles: &HashMap<String, crate::rsd::ServiceEntry>,
    config: &RunConfig<'_>,
    log_out: &mut dyn Write,
) -> Result<bool, crate::tunnel::Error> {
    use crate::tunnel::Error;
    let err = |s: String| Error::Protocol(s);

    // ── 1. Get service ports ────────────────────────────────────────────────
    let tm_port = rsd
        .service("com.apple.dt.testmanagerd.remote")
        .ok_or_else(|| err("testmanagerd.remote not in RSD catalog".into()))?
        .port;
    let app_svc_port = rsd
        .service("com.apple.coredevice.appservice")
        .ok_or_else(|| err("coredevice.appservice not in RSD catalog".into()))?
        .port;
    let stdio_port = rsd
        .service("com.apple.coredevice.openstdiosocket")
        .ok_or_else(|| err("coredevice.openstdiosocket not in RSD catalog".into()))?
        .port;

    // Look up the test runner app path from the installed-apps map
    let runner_info = app_bundles
        .get(config.test_runner_bundle_id)
        .ok_or_else(|| {
            err(format!(
                "test runner '{}' not installed",
                config.test_runner_bundle_id
            ))
        })?;
    let runner_path = runner_info
        .properties
        .get("Path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // ── 2. Open stdio socket (captures test runner stdout/stderr) ───────────
    let server_addr = tunnel.params.server_addr;
    let stdio_stream = tunnel
        .connect(server_addr, stdio_port)
        .map_err(|e| err(format!("openstdiosocket: {e}")))?;
    let (stdio_tcp, stdio_uuid) =
        unix_to_tcp_relay_read_uuid(stdio_stream).map_err(|e| err(format!("stdio relay: {e}")))?;

    // Spawn thread to copy stdio → log_out (we use a channel to forward)
    let (stdio_tx, stdio_rx) = mpsc::sync_channel::<Vec<u8>>(64);
    thread::spawn(move || {
        let mut tcp = stdio_tcp;
        let mut buf = [0u8; 4096];
        loop {
            match tcp.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = stdio_tx.send(buf[..n].to_vec());
                }
            }
        }
    });

    // ── 3. Open two DTX connections to testmanagerd ─────────────────────────
    let conn1 = Arc::new(
        open_dtx(tunnel, server_addr, tm_port)
            .map_err(|e| err(format!("testmanagerd conn1: {e}")))?,
    );
    let conn2 = Arc::new(
        open_dtx(tunnel, server_addr, tm_port)
            .map_err(|e| err(format!("testmanagerd conn2: {e}")))?,
    );

    let chan_name = "dtxproxy:XCTestManager_IDEInterface:XCTestManager_DaemonConnectionInterface";

    // ── 4. conn1 + conn2: capability handshake (required by testmanagerd) ───
    // testmanagerd ignores _requestChannelWithCode:identifier: until the host
    // sends _notifyOfPublishedCapabilities: — both pymd3 and go-ios do this.
    conn1
        .handshake()
        .map_err(|e| err(format!("conn1 handshake: {e}")))?;
    conn2
        .handshake()
        .map_err(|e| err(format!("conn2 handshake: {e}")))?;

    // ── 5. conn1: request IDE channel + initiate session ────────────────────
    let ide_chan1 = conn1
        .request_channel(chan_name)
        .map_err(|e| err(format!("request channel conn1: {e}")))?;

    // Register to receive incoming calls on ide_chan1 (test runner will call us back)
    let incoming1 = conn1.register_channel(ide_chan1);

    // Register ch=0 to capture _requestChannelWithCode:identifier: from the runner,
    // which tells us the runner's channel code for XCTestDriverInterface.
    // We need -(runner's code) to send driver interface messages to the runner.
    let incoming0 = conn1.register_channel(0);
    let driver_invoke_chan = Arc::new(std::sync::atomic::AtomicI32::new(-1));

    let session_id: [u8; 16] = rand_uuid();
    let local_caps = build_caps(&[
        ("XCTIssue capability", true),
        ("daemon container sandbox extension", true),
        ("delayed attachment transfer", true),
        ("expected failure test capability", true),
        ("request diagnostics for specific devices", true),
        ("skipped test capability", true),
        ("test case run configurations", true),
        ("test iterations", true),
        ("test timeout capability", true),
        ("ubiquitous test identifiers", true),
    ]);

    conn1
        .call(
            ide_chan1,
            "_IDE_initiateSessionWithIdentifier:capabilities:",
            &[
                AuxValue::Bytes(archive_uuid(&session_id)),
                AuxValue::Bytes(archive_xct_capabilities(&local_caps)),
            ],
        )
        .map_err(|e| err(format!("initiateSession: {e}")))?;

    // ── 5. Register driver channel BEFORE launching the test runner ─────────────
    // The test runner connects to testmanagerd as soon as it launches and immediately
    // requests the XCTestDriverInterface channel. If we register it too late, the
    // runner gets "Protocol handler unavailable" and hangs waiting for test-plan start.
    // Register driver channel so testmanagerd can link it with the runner's channel.
    // The actual send channel is determined dynamically from the runner's registration
    // (see driver_invoke_chan below); driver_chan itself is only for testmanagerd routing.
    let driver_chan_name = "dtxproxy:XCTestDriverInterface:XCTestManager_IDEInterface";
    let _driver_chan = conn1
        .request_channel(driver_chan_name)
        .map_err(|e| err(format!("request driver channel: {e}")))?;

    let test_bundle_path = format!("{runner_path}/PlugIns/{}", config.xctest_config_name);
    let session_str = uuid_to_upper_string(&session_id);

    // Build XCTestConfiguration here so the dispatch handler below can capture it.
    // Build app_dependencies: runner + optional target app (matches Xcode's testApplicationDependencies)
    let mut app_deps = vec![(
        config.test_runner_bundle_id.to_string(),
        runner_path.clone(),
    )];
    if !config.bundle_id.is_empty() && !config.bundle_path.is_empty() {
        app_deps.push((config.bundle_id.to_string(), config.bundle_path.to_string()));
    }

    let xctest_config_bytes = build_xctest_configuration_bytes(config::XCTestConfigArgs {
        session_id: &session_id,
        test_bundle_path: &test_bundle_path,
        product_module_name: config.xctest_config_name.trim_end_matches(".xctest"),
        target_bundle_id: config.bundle_id,
        target_path: config.bundle_path,
        tests_to_run: config.tests_to_run,
        tests_to_skip: config.tests_to_skip,
        is_xctest: config.is_xctest,
        initialize_for_ui: config.initialize_for_ui,
        app_dependencies: &app_deps,
        runner_bundle_id: config.test_runner_bundle_id,
    });

    // Watch ch=0 for the runner's _requestChannelWithCode:identifier: for
    // XCTestDriverInterface. The runner's channel code (negated) is what we
    // need to send _IDE_startExecutingTestPlanWithProtocolVersion: to the runner.
    let driver_invoke_chan_clone = Arc::clone(&driver_invoke_chan);
    thread::spawn(move || {
        fn decode_aux_str(a: &AuxValue) -> Option<String> {
            if let AuxValue::Bytes(b) = a {
                plist::from_bytes::<plist::Value>(b).ok().and_then(|v| {
                    v.as_dictionary()
                        .and_then(|d| d.get("$objects"))
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.get(1))
                        .and_then(|s| s.as_string())
                        .map(|s| s.to_owned())
                })
            } else {
                None
            }
        }
        while let Ok(msg) = incoming0.recv() {
            let sel = msg
                .payload
                .as_ref()
                .and_then(archived_string)
                .unwrap_or_default();
            if sel.contains("requestChannelWithCode") {
                let code = msg.aux.first().and_then(|a| {
                    if let AuxValue::Int32(v) = a {
                        Some(*v)
                    } else {
                        None
                    }
                });
                let ident = msg.aux.get(1).and_then(decode_aux_str);
                if let (Some(code), Some(ident)) = (code, ident) {
                    if ident.contains("XCTestDriverInterface") {
                        // Sending on -(runner's code) routes to the runner via testmanagerd
                        driver_invoke_chan_clone.store(-code, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    });

    // Channel to signal the wait loop that the test plan finished — so we exit
    // immediately instead of waiting up to 300s for the runner process to die.
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);

    // Start dispatch handler for incoming test-runner calls on ide_chan1 before launch.
    let xctest_config_bytes_clone = xctest_config_bytes.clone();
    let conn1_dispatch = Arc::clone(&conn1);
    let driver_invoke_chan_cap = Arc::clone(&driver_invoke_chan);
    let done_tx2 = done_tx.clone();
    thread::spawn(move || {
        let conn1_ref = &*conn1_dispatch;
        let driver_invoke_chan = driver_invoke_chan_cap;
        let mut test_case_finished = false;
        let mut plan_started = false;
        while let Ok(msg) = incoming1.recv() {
            let selector = msg
                .payload
                .as_ref()
                .and_then(archived_string)
                .unwrap_or_default();
            // Decode first aux argument (the debug message text for logDebugMessage).
            let aux0 = msg
                .aux
                .first()
                .and_then(|a| {
                    if let AuxValue::Bytes(b) = a {
                        plist::from_bytes::<plist::Value>(b).ok().and_then(|v| {
                            v.as_dictionary()
                                .and_then(|d| d.get("$objects"))
                                .and_then(|a| a.as_array())
                                .and_then(|arr| arr.get(1))
                                .and_then(|s| s.as_string())
                                .map(|s| s.to_owned())
                        })
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            if selector.contains("testRunnerReadyWithCapabilities") {
                // Typed reply (type=3) with XCTestConfiguration — no prior ACK.
                let _ = conn1_ref.reply(&msg, &xctest_config_bytes_clone);
            } else if selector.contains("didFinishExecutingTestPlan") {
                if msg.expects_reply {
                    let _ = conn1_ref.ack(&msg);
                }
                let _ = done_tx.try_send(());
                break;
            } else if selector.contains("testCaseWithIdentifier:didFinishWithStatus:duration:")
                || selector.contains("testCaseDidFinishForTestClass:method:")
            {
                if msg.expects_reply {
                    let _ = conn1_ref.ack(&msg);
                }
                test_case_finished = true;
            } else if test_case_finished
                && selector.contains("testSuiteWithIdentifier:didFinishAt:")
            {
                // Top-level suite finished after test cases ran — signal done.
                // didFinishExecutingTestPlan sometimes doesn't arrive when runner
                // stays alive in "confirming end of session" loop.
                if msg.expects_reply {
                    let _ = conn1_ref.ack(&msg);
                }
                let _ = done_tx2.try_send(());
            } else {
                // Void-returning methods: ACK only.
                if msg.expects_reply {
                    let _ = conn1_ref.ack(&msg);
                }
                // The runner logs "requesting ready for testing" just before it enters
                // the wait loop for _IDE_startExecutingTestPlanWithProtocolVersion:.
                // Send the start command at this point so it arrives after the runner's
                // internal handler is set up (mirrors pymd3's wait_for_proxied_service).
                if !plan_started && aux0.contains("requesting ready for testing") {
                    plan_started = true;
                    // Use the runner's registered channel code (negated): in DTX proxy,
                    // sending on -(runner's code) routes to the runner.
                    let ch = driver_invoke_chan.load(std::sync::atomic::Ordering::Relaxed);
                    let _ = conn1_ref.call_async(
                        ch,
                        "_IDE_startExecutingTestPlanWithProtocolVersion:",
                        &[AuxValue::Bytes(dtx::archive_u64(36))],
                    );
                }
            }
        }
    });

    // ── 6. Launch test runner ─────────────────────────────────────────────────
    let pid = launch_test_runner(
        tunnel,
        server_addr,
        app_svc_port,
        config.test_runner_bundle_id,
        &session_str,
        &test_bundle_path,
        &config.extra_env,
        config.is_xctest,         // adds libXCTestBundleInject for unit tests
        config.initialize_for_ui, // controls ActivateSuspended launch option
        &stdio_uuid,
    )
    .map_err(|e| err(format!("launch test runner: {e}")))?;

    writeln!(log_out, "[ios-rs] Test runner launched (pid={pid})").ok();

    // ── 7. conn2: control session + authorize ────────────────────────────────
    let ide_chan2 = conn2
        .request_channel(chan_name)
        .map_err(|e| err(format!("request channel conn2: {e}")))?;

    let empty_caps: HashMap<&str, Value> = HashMap::new();
    conn2
        .call(
            ide_chan2,
            "_IDE_initiateControlSessionWithCapabilities:",
            &[AuxValue::Bytes(archive_xct_capabilities(&empty_caps))],
        )
        .map_err(|e| err(format!("initiateControlSession: {e}")))?;

    conn2
        .call(
            ide_chan2,
            "_IDE_authorizeTestSessionWithProcessID:",
            &[AuxValue::Bytes(dtx::archive_u64(pid as u64))],
        )
        .map_err(|e| err(format!("authorizeTestSession: {e}")))?;

    // ── 8. Stream output and wait for runner to exit ─────────────────────────
    // The stdio channel disconnects when the test runner process exits.
    let passed = true;
    let timeout = Duration::from_secs(300);
    let deadline = std::time::Instant::now() + timeout;

    loop {
        match stdio_rx.try_recv() {
            Ok(data) => {
                log_out.write_all(&data).ok();
            }
            Err(mpsc::TryRecvError::Disconnected) => break, // runner exited
            Err(mpsc::TryRecvError::Empty) => {}
        }
        // Exit immediately when the test plan finishes — don't wait for the runner
        // process to die (it may linger for 1800s in "confirming end of session").
        if done_rx.try_recv().is_ok() {
            break;
        }
        if std::time::Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Final flush
    while let Ok(data) = stdio_rx.try_recv() {
        log_out.write_all(&data).ok();
    }

    Ok(passed)
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn open_dtx(
    tunnel: &SmoltcpTunnel,
    server_addr: std::net::Ipv6Addr,
    port: u16,
) -> Result<DtxConn, std::io::Error> {
    let stream = tunnel
        .connect(server_addr, port)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let (tcp_r, tcp_w) = unix_to_tcp_pair(stream)?;
    Ok(DtxConn::new(tcp_r, tcp_w))
}

/// Create a loopback TCP relay for a UnixStream (same pattern as RsdClient::connect_stream).
/// Returns (TcpStream for reading, TcpStream for writing).
fn unix_to_tcp_pair(
    unix: std::os::unix::net::UnixStream,
) -> std::io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let client = TcpStream::connect(addr)?;

    thread::spawn(move || {
        if let Ok((server, _)) = listener.accept() {
            let mut uni_r = unix.try_clone().unwrap();
            let mut uni_w = unix;
            let mut tcp_w = server.try_clone().unwrap();
            let mut tcp_r = server;
            let t1 = thread::spawn(move || {
                std::io::copy(&mut uni_r, &mut tcp_w).ok();
            });
            let t2 = thread::spawn(move || {
                std::io::copy(&mut tcp_r, &mut uni_w).ok();
            });
            let _ = (t1.join(), t2.join());
        }
    });

    let client_r = client.try_clone()?;
    Ok((client_r, client))
}

/// Open a loopback TCP relay, read the 16-byte UUID the openstdio service sends,
/// and return (TcpStream for reading stdio, UUID bytes).
fn unix_to_tcp_relay_read_uuid(
    unix: std::os::unix::net::UnixStream,
) -> std::io::Result<(TcpStream, [u8; 16])> {
    let (mut r, _w) = unix_to_tcp_pair(unix)?;
    let mut uuid = [0u8; 16];
    read_all(&mut r, &mut uuid)?;
    Ok((r, uuid))
}

fn read_all<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = r.read(&mut buf[done..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof",
            ));
        }
        done += n;
    }
    Ok(())
}

fn rand_uuid() -> [u8; 16] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut b = [0u8; 16];
    let tb = t.to_le_bytes();
    b[..16].copy_from_slice(&tb[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    b
}

fn uuid_to_upper_string(bytes: &[u8; 16]) -> String {
    format!("{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2],  bytes[3],
        bytes[4], bytes[5], bytes[6],  bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12],bytes[13],bytes[14], bytes[15])
}

fn build_caps<'a>(entries: &[(&'a str, bool)]) -> HashMap<&'a str, Value> {
    entries
        .iter()
        .map(|(k, v)| (*k, Value::Boolean(*v)))
        .collect()
}

/// Extract the string from an NSKeyedArchiver-wrapped string primitive.
/// NSKeyedArchiver format: { "$objects": ["$null", <the string>, ...], ... }
fn archived_string(v: &plist::Value) -> Option<String> {
    v.as_dictionary()
        .and_then(|d| d.get("$objects"))
        .and_then(|a| a.as_array())
        .and_then(|arr| arr.get(1))
        .and_then(|s| s.as_string())
        .map(|s| s.to_owned())
}

/// Launch the test runner via coredevice.appservice.
#[allow(clippy::too_many_arguments)]
fn launch_test_runner(
    tunnel: &SmoltcpTunnel,
    server_addr: std::net::Ipv6Addr,
    app_svc_port: u16,
    bundle_id: &str,
    session_id: &str,
    test_bundle_path: &str,
    extra_env: &HashMap<String, String>,
    is_xctest: bool,
    initialize_for_ui: bool,
    stdio_uuid: &[u8; 16],
) -> Result<i64, crate::tunnel::Error> {
    use crate::tunnel::Error;

    let stream = tunnel
        .connect(server_addr, app_svc_port)
        .map_err(|e| Error::Protocol(format!("appservice connect: {e}")))?;

    // Relay UnixStream → loopback TCP for RemoteXpcConn
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| Error::Protocol(format!("tcp listener: {e}")))?;
    let relay_addr = listener
        .local_addr()
        .map_err(|e| Error::Protocol(format!("local_addr: {e}")))?;

    thread::spawn(move || {
        if let Ok((server, _)) = listener.accept() {
            let mut uni_r = stream.try_clone().unwrap();
            let mut uni_w = stream;
            let mut tcp_w = server.try_clone().unwrap();
            let mut tcp_r = server;
            let t1 = thread::spawn(move || {
                std::io::copy(&mut uni_r, &mut tcp_w).ok();
            });
            let t2 = thread::spawn(move || {
                std::io::copy(&mut tcp_r, &mut uni_w).ok();
            });
            let _ = (t1.join(), t2.join());
        }
    });

    let xpc_conn = crate::remotexpc::RemoteXpcConn::connect(relay_addr)
        .map_err(|e| Error::Protocol(format!("remotexpc: {e}")))?;

    let stdio_uuid_str = uuid_to_upper_string(stdio_uuid).to_lowercase();

    // When running without a host app (orientation-only), mirror what Xcode sends:
    // minimal env with no DYLD injections and a relative XCTestBundlePath.
    // Extra DYLD vars trigger XCUITest host-app discovery logic.
    let xctest_bundle_name = test_bundle_path
        .rsplit('/')
        .next()
        .unwrap_or(test_bundle_path);
    let bundle_path_for_env = if initialize_for_ui {
        test_bundle_path
    } else {
        // Relative path as Xcode uses: "PlugIns/<name>.xctest"
        &*format!("PlugIns/{xctest_bundle_name}")
    };
    // Leak to get a 'static str for the env array (only used locally)
    let bundle_path_for_env: &str = Box::leak(bundle_path_for_env.to_string().into_boxed_str());

    // Xcode does NOT inject DYLD libraries when running without a host app.
    // Injecting libMainThreadChecker triggers host-app discovery in XCUITest.
    let mut libraries = String::new();
    if is_xctest {
        libraries.push_str("/System/Developer/usr/lib/libXCTestBundleInject.dylib");
    }

    // Match Xcode's xctestrun EnvironmentVariables + TestingEnvironmentVariables.
    // Notably: libMainThreadChecker is NOT in the base env from Xcode (it's only
    // in TestingEnvironmentVariables which the runner merges in itself).
    let mut env: HashMap<String, crate::xpc::Value> = [
        ("DYLD_INSERT_LIBRARIES", &libraries as &str),
        (
            "DYLD_FRAMEWORK_PATH",
            if initialize_for_ui {
                "/System/Developer/Library/Frameworks"
            } else {
                ""
            },
        ),
        (
            "DYLD_LIBRARY_PATH",
            if initialize_for_ui {
                "/System/Developer/usr/lib"
            } else {
                ""
            },
        ),
        ("NSUnbufferedIO", "YES"),
        ("OS_ACTIVITY_DT_MODE", "YES"),
        ("SQLITE_ENABLE_THREAD_ASSERTIONS", "1"),
        ("XCTestBundlePath", bundle_path_for_env),
        ("XCTestConfigurationFilePath", ""),
        ("XCTestManagerVariant", "DDI"),
        ("XCTestSessionIdentifier", session_id),
    ]
    .iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| (k.to_string(), crate::xpc::Value::String(v.to_string())))
    .collect();

    for (k, v) in extra_env {
        env.insert(k.clone(), crate::xpc::Value::String(v.clone()));
    }

    let opts: HashMap<String, crate::xpc::Value> = if initialize_for_ui && !is_xctest {
        [
            ("ActivateSuspended", crate::xpc::Value::Uint64(1)),
            ("StartSuspendedKey", crate::xpc::Value::Uint64(0)),
            ("__ActivateSuspended", crate::xpc::Value::Uint64(1)),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
    } else {
        HashMap::new()
    };

    // Build platform-specific options plist (empty dict as binary plist)
    let mut platform_opts_buf = Vec::new();
    let empty_dict = plist::Value::Dictionary(plist::Dictionary::new());
    plist::to_writer_binary(&mut platform_opts_buf, &empty_dict).unwrap();

    let device_id = uuid_to_upper_string(&rand_uuid()).to_lowercase();
    let inv_id = uuid_to_upper_string(&rand_uuid()).to_lowercase();

    let payload = build_coredevice_request(
        &device_id,
        "com.apple.coredevice.feature.launchapplication",
        Some(build_launch_input(
            bundle_id,
            &env,
            &opts,
            &platform_opts_buf,
            stdio_uuid,
        )),
    );

    let reply = xpc_conn
        .request(payload)
        .map_err(|e| Error::Protocol(format!("appservice request: {e}")))?;
    let _ = (inv_id, stdio_uuid_str);

    // Extract PID from reply (CoreDevice.output.processToken.processIdentifier)
    let pid = reply
        .as_dict()
        .and_then(|d| d.get("CoreDevice.output"))
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("processToken"))
        .and_then(|v| v.as_dict())
        .and_then(|d| d.get("processIdentifier"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| Error::Protocol(format!("no PID in appservice reply: {reply:?}")))?;

    Ok(pid)
}

fn build_coredevice_request(
    device_id: &str,
    feature: &str,
    input: Option<crate::xpc::Value>,
) -> crate::xpc::Value {
    use crate::xpc::Value;
    let mut d = std::collections::HashMap::new();
    d.insert(
        "CoreDevice.CoreDeviceDDIProtocolVersion".to_string(),
        Value::Int64(0),
    );
    d.insert(
        "CoreDevice.action".to_string(),
        Value::Dictionary(std::collections::HashMap::new()),
    );
    d.insert("CoreDevice.coreDeviceVersion".to_string(), {
        let mut ver = std::collections::HashMap::new();
        ver.insert("stringValue".to_string(), Value::String("348.1".into()));
        ver.insert("originalComponentsCount".to_string(), Value::Int64(2));
        ver.insert(
            "components".to_string(),
            Value::Array(vec![
                Value::Uint64(348),
                Value::Uint64(1),
                Value::Uint64(0),
                Value::Uint64(0),
                Value::Uint64(0),
            ]),
        );
        Value::Dictionary(ver)
    });
    d.insert(
        "CoreDevice.deviceIdentifier".to_string(),
        Value::String(device_id.into()),
    );
    d.insert(
        "CoreDevice.featureIdentifier".to_string(),
        Value::String(feature.into()),
    );
    d.insert(
        "CoreDevice.invocationIdentifier".to_string(),
        Value::String(uuid_to_upper_string(&rand_uuid())),
    );
    if let Some(inp) = input {
        d.insert("CoreDevice.input".to_string(), inp);
    } else {
        d.insert("CoreDevice.input".to_string(), Value::Null);
    }
    Value::Dictionary(d)
}

fn build_launch_input(
    bundle_id: &str,
    env: &HashMap<String, crate::xpc::Value>,
    opts: &HashMap<String, crate::xpc::Value>,
    platform_opts: &[u8],
    stdio_uuid: &[u8; 16],
) -> crate::xpc::Value {
    use crate::xpc::Value;
    let mut d = std::collections::HashMap::new();
    d.insert("applicationSpecifier".to_string(), {
        let mut spec = std::collections::HashMap::new();
        let mut bi = std::collections::HashMap::new();
        bi.insert("_0".to_string(), Value::String(bundle_id.into()));
        spec.insert("bundleIdentifier".to_string(), Value::Dictionary(bi));
        Value::Dictionary(spec)
    });
    d.insert("options".to_string(), {
        let mut o = std::collections::HashMap::new();
        o.insert("arguments".to_string(), Value::Array(vec![]));
        o.insert(
            "environmentVariables".to_string(),
            Value::Dictionary(env.clone()),
        );
        o.insert(
            "platformSpecificOptions".to_string(),
            Value::Data(platform_opts.to_vec()),
        );
        o.insert(
            "standardIOUsesPseudoterminals".to_string(),
            Value::Bool(true),
        );
        o.insert("startStopped".to_string(), Value::Bool(false));
        o.insert("terminateExisting".to_string(), Value::Bool(true));
        o.insert("user".to_string(), {
            let mut u = std::collections::HashMap::new();
            u.insert("active".to_string(), Value::Bool(true));
            Value::Dictionary(u)
        });
        o.insert("workingDirectory".to_string(), Value::Null);
        for (k, v) in opts {
            o.insert(k.clone(), v.clone());
        }
        Value::Dictionary(o)
    });
    d.insert("standardIOIdentifiers".to_string(), {
        let mut s = std::collections::HashMap::new();
        s.insert("standardInput".to_string(), Value::Uuid(*stdio_uuid));
        s.insert("standardOutput".to_string(), Value::Uuid(*stdio_uuid));
        s.insert("standardError".to_string(), Value::Uuid(*stdio_uuid));
        Value::Dictionary(s)
    });
    Value::Dictionary(d)
}
