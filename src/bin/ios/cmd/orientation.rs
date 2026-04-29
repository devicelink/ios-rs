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
///   ios orientation set portrait  [--bundle-id=<app_under_test>]
///                                  --runner-bundle-id=<com.example.OrientationHelper.xctrunner>
///                                  --xctest-config=ios-rs-helperUITests.xctest
use anyhow::{bail, Result};
use ios_rs::lockdown::services::{Orientation, SpringBoardClient};
use ios_rs::tunnel::ConnectionMode;

use crate::cmd::open_session;


pub fn get(udid: Option<&str>, mode: ConnectionMode) -> Result<()> {
    let mut session = open_session(udid, mode)?;
    let mut sbs = SpringBoardClient::connect(session.lockdown())
        .map_err(|e| anyhow::anyhow!("springboardservices: {e}"))?;
    let orientation = sbs.get_orientation()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{orientation}  ({})", orientation as u8);
    Ok(())
}

pub fn set(
    udid:              Option<&str>,
    direction:         &str,
    bundle_id:         Option<&str>,
    runner_bundle_id:  &str,
    xctest_config:     &str,
) -> Result<()> {
    // Validate the direction string up-front
    Orientation::parse(direction)
        .ok_or_else(|| anyhow::anyhow!(
            "unknown orientation '{direction}'. \
             Valid values: portrait, portrait_upside_down, landscape_left, landscape_right"
        ))?;

    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    if !session.is_rsd() {
        bail!("orientation set requires iOS 17.4+ RSD path (CDTunnel)");
    }

    let rsd = session.connect_rsd()
        .map_err(|e| anyhow::anyhow!("RSD: {e}"))?;

    // Build the app bundles map for the test runner path lookup
    let app_bundles = {
        use ios_rs::lockdown::services::{AppType, InstallationProxy};
        use ios_rs::rsd::ServiceEntry;
        use ios_rs::xpc::Value;
        use std::collections::HashMap;

        let stream = session.connect_rsd_shim(
                "com.apple.mobile.installation_proxy.shim.remote")
            .map_err(|e| anyhow::anyhow!("installation_proxy shim: {e}"))?;
        let mut proxy = InstallationProxy::from_stream(stream);
        let apps = proxy.list_apps(AppType::Any)
            .map_err(|e| anyhow::anyhow!("list apps: {e}"))?;

        apps.into_iter().map(|app| {
            let mut props = HashMap::new();
            props.insert("Path".to_string(), Value::String(app.path));
            (app.bundle_id, ServiceEntry { port: 0, uses_remote_xpc: false, properties: props })
        }).collect::<HashMap<_, _>>()
    };

    let tunnel = session.smoltcp_tunnel_ref()
        .ok_or_else(|| anyhow::anyhow!("no tunnel"))?;

    let mut env = std::collections::HashMap::new();
    env.insert("ORIENTATION".to_string(), direction.to_string());

    let config = ios_rs::xctest::RunConfig {
        bundle_id:              bundle_id.unwrap_or(""),
        test_runner_bundle_id:  runner_bundle_id,
        xctest_config_name:     xctest_config,
        tests_to_run:           &[],
        tests_to_skip:          &[],
        is_xctest:              false,
        extra_env:              env,
    };

    eprintln!("[ios-rs] Setting orientation to '{direction}' via XCUITest…");
    ios_rs::xctest::run(tunnel, &rsd, &app_bundles, &config, &mut std::io::stderr())
        .map_err(|e| anyhow::anyhow!("test run: {e}"))?;

    println!("Orientation set to '{direction}'.");
    Ok(())
}
