/// Screen orientation commands.
///
/// # Getting orientation
///
/// Reads the current interface orientation directly from SpringBoard via
/// `com.apple.springboardservices`.
///
/// # Setting orientation
///
/// iOS provides no public API to force orientation from outside the device.
/// The only reliable path is a short-lived XCUITest that calls
/// `XCUIDevice.shared.orientation = .X` and exits.
///
/// ## One-time setup: build & install the orientation helper
///
/// Create an XCTest target with this Swift source and sign/install it once:
///
/// ```swift
/// // OrientationHelperUITests.swift
/// import XCTest
///
/// class OrientationHelperTests: XCTestCase {
///     func testSetOrientation() throws {
///         let raw = ProcessInfo.processInfo.environment["ORIENTATION"] ?? "portrait"
///         XCUIDevice.shared.orientation = orientation(from: raw)
///         Thread.sleep(forTimeInterval: 0.3)
///     }
///     private func orientation(from s: String) -> UIDeviceOrientation {
///         switch s.lowercased() {
///         case "portrait_upside_down": return .portraitUpsideDown
///         case "landscape_left":       return .landscapeLeft
///         case "landscape_right":      return .landscapeRight
///         default:                     return .portrait
///         }
///     }
/// }
/// ```
///
/// Build in Xcode, then:
///   ios apps install OrientationHelper.ipa
///
/// ## Usage
///   ios orientation get
///   ios orientation set portrait
///
/// The orientation helper runner must be installed once. Do NOT pass --bundle-id;
/// doing so causes the named app to launch as the UITest target, which is disruptive.
use anyhow::{bail, Result};
use ios_rs::lockdown::services::{Orientation, SpringBoardClient};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

#[derive(serde::Serialize)]
struct OrientationInfo {
    orientation: String,
}

pub fn get(udid: Option<&str>, mode: ConnectionMode, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut sbs = SpringBoardClient::connect(session.lockdown())
        .map_err(|e| anyhow::anyhow!("springboardservices: {e}"))?;
    let orientation = sbs.get_orientation().map_err(|e| anyhow::anyhow!("{e}"))?;

    if output.is_json() {
        return print_json(&OrientationInfo {
            orientation: orientation.to_string(),
        });
    }

    println!("{orientation}  ({})", orientation as u8);
    Ok(())
}

pub fn set(
    udid: Option<&str>,
    direction: &str,
    bundle_id: Option<&str>,
    runner_bundle_id: &str,
    xctest_config: &str,
    output: OutputMode,
) -> Result<()> {
    let target = Orientation::parse(direction).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown orientation '{direction}'. \
             Valid values: portrait, portrait_upside_down, landscape_left, landscape_right"
        )
    })?;

    // Try direct SpringBoard API first — instant, no XCUITest, no host app launch.
    {
        let mut session = open_session(udid, ConnectionMode::Legacy)?;
        if let Ok(mut sbs) = SpringBoardClient::connect(session.lockdown()) {
            if sbs.set_orientation(target).is_ok() {
                if output.is_json() {
                    return print_json(&ActionResult::with_msg(format!(
                        "Orientation set to '{direction}'."
                    )));
                }
                println!("Orientation set to '{direction}'.");
                return Ok(());
            }
        }
    }

    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    if !session.is_rsd() {
        bail!("orientation set requires iOS 17.4+ RSD path (CDTunnel)");
    }

    let rsd = session
        .connect_rsd()
        .map_err(|e| anyhow::anyhow!("RSD: {e}"))?;

    // Build the app bundles map for the test runner path lookup
    let app_bundles = {
        use ios_rs::lockdown::services::{AppType, InstallationProxy};
        use ios_rs::rsd::ServiceEntry;
        use ios_rs::xpc::Value;
        use std::collections::HashMap;

        let stream = session
            .connect_rsd_shim("com.apple.mobile.installation_proxy.shim.remote")
            .map_err(|e| anyhow::anyhow!("installation_proxy shim: {e}"))?;
        let mut proxy = InstallationProxy::from_stream(stream);
        let apps = proxy
            .list_apps(AppType::Any)
            .map_err(|e| anyhow::anyhow!("list apps: {e}"))?;

        apps.into_iter()
            .map(|app| {
                let mut props = HashMap::new();
                props.insert("Path".to_string(), Value::String(app.path));
                (
                    app.bundle_id,
                    ServiceEntry {
                        port: 0,
                        uses_remote_xpc: false,
                        properties: props,
                    },
                )
            })
            .collect::<HashMap<_, _>>()
    };

    // Auto-detect the current foreground app before taking the tunnel reference.
    // Skip system apps (SpringBoard = home screen, etc.) — XCUITest can't usefully
    // attach to them and it would trigger unexpected app launches.
    let foreground_bid: Option<String> = if bundle_id.is_none() {
        use ios_rs::lockdown::services::SpringBoardClient;
        SpringBoardClient::connect(session.lockdown())
            .ok()
            .and_then(|mut sbs| sbs.get_foreground_app().ok())
            .filter(|bid| !bid.starts_with("com.apple.springboard") && !bid.is_empty())
    } else {
        None
    };

    let tunnel = session
        .smoltcp_tunnel_ref()
        .ok_or_else(|| anyhow::anyhow!("no tunnel"))?;

    let mut env = std::collections::HashMap::new();
    env.insert("ORIENTATION".to_string(), direction.to_string());
    let target_bid = bundle_id.unwrap_or_else(|| foreground_bid.as_deref().unwrap_or(""));
    if !target_bid.is_empty() {
        eprintln!("[ios-rs] Using foreground app: {target_bid}");
    }
    let target_path = if !target_bid.is_empty() {
        app_bundles
            .get(target_bid)
            .and_then(|e| e.properties.get("Path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    let config = ios_rs::xctest::RunConfig {
        bundle_id: target_bid,
        bundle_path: &target_path,
        test_runner_bundle_id: runner_bundle_id,
        xctest_config_name: xctest_config,
        tests_to_run: &[],
        tests_to_skip: &[],
        is_xctest: false,
        initialize_for_ui: true,
        extra_env: env,
    };

    eprintln!("[ios-rs] Setting orientation to '{direction}' via XCUITest…");
    ios_rs::xctest::run(tunnel, &rsd, &app_bundles, &config, &mut std::io::stderr())
        .map_err(|e| anyhow::anyhow!("test run: {e}"))?;

    if output.is_json() {
        print_json(&ActionResult::with_msg(format!(
            "Orientation set to '{direction}'."
        )))
    } else {
        println!("Orientation set to '{direction}'.");
        Ok(())
    }
}
