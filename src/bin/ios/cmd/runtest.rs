use std::collections::HashMap;
use std::io;

use anyhow::{bail, Result};
use ios_rs::tunnel::ConnectionMode;
use ios_rs::xctest::{run as xctest_run, RunConfig};

use crate::cmd::open_session;

#[allow(clippy::too_many_arguments)]
pub fn run_test(
    udid: Option<&str>,
    bundle_id: &str,
    test_runner_bundle_id: &str,
    xctest_config: &str,
    tests_to_run: Vec<String>,
    tests_to_skip: Vec<String>,
    is_xctest: bool,
    extra_env: Vec<String>,
) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;

    if !session.is_rsd() {
        bail!("runtest requires iOS 17.4+ with RSD path (CDTunnel). Use --rsd or check device version.");
    }

    let rsd = session
        .connect_rsd()
        .map_err(|e| anyhow::anyhow!("RSD connect: {e}"))?;

    // Build environment map from installed apps — do all mutable borrows before taking tunnel ref
    let app_bundles: HashMap<String, ios_rs::rsd::ServiceEntry> = {
        use ios_rs::lockdown::services::{AppType, InstallationProxy};
        let stream = session
            .connect_rsd_shim("com.apple.mobile.installation_proxy.shim.remote")
            .map_err(|e| anyhow::anyhow!("installation_proxy shim: {e}"))?;
        let mut proxy = InstallationProxy::from_stream(stream);
        let apps = proxy
            .list_apps(AppType::Any)
            .map_err(|e| anyhow::anyhow!("list apps: {e}"))?;

        // Convert to a map keyed by bundle ID with a fake ServiceEntry holding Path
        apps.into_iter()
            .map(|app| {
                let mut props = HashMap::new();
                props.insert(
                    "Path".to_string(),
                    ios_rs::xpc::Value::String(app.path.clone()),
                );
                (
                    app.bundle_id,
                    ios_rs::rsd::ServiceEntry {
                        port: 0,
                        uses_remote_xpc: false,
                        properties: props,
                    },
                )
            })
            .collect()
    };

    // Now get tunnel ref (immutable borrow — all mutable borrows above are done)
    let tunnel = session
        .smoltcp_tunnel_ref()
        .ok_or_else(|| anyhow::anyhow!("no tunnel"))?;

    // Parse extra env
    let mut env_map = HashMap::new();
    for kv in &extra_env {
        if let Some((k, v)) = kv.split_once('=') {
            env_map.insert(k.to_string(), v.to_string());
        }
    }

    let config = RunConfig {
        bundle_id,
        bundle_path: "",
        test_runner_bundle_id,
        xctest_config_name: xctest_config,
        tests_to_run: &tests_to_run,
        tests_to_skip: &tests_to_skip,
        is_xctest,
        initialize_for_ui: !is_xctest,
        extra_env: env_map,
    };

    let mut stdout = io::stdout();
    let passed = xctest_run(tunnel, &rsd, &app_bundles, &config, &mut stdout)
        .map_err(|e| anyhow::anyhow!("test run failed: {e}"))?;

    if passed {
        println!("\n✓ Tests passed");
        Ok(())
    } else {
        bail!("✗ Tests failed")
    }
}

pub fn run_wda(
    udid: Option<&str>,
    bundle_id: &str,
    test_runner_bundle_id: &str,
    xctest_config: &str,
) -> Result<()> {
    // WDA is just a regular UI test run that blocks until killed
    run_test(
        udid,
        bundle_id,
        test_runner_bundle_id,
        xctest_config,
        vec![],
        vec![],
        false,
        vec![],
    )
}
