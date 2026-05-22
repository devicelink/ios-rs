mod cmd;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ios_rs::tunnel::ConnectionMode;

// ureq uses rustls 0.23 which requires an explicit crypto provider.
// Install ring before any TLS connections are made.
#[cfg(feature = "cli")]
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[derive(Parser)]
#[command(name = "ios", about = "Interact with iOS devices via usbmuxd")]
struct Cli {
    /// Force legacy usbmux → lockdownd path even on iOS 17+ devices.
    /// Also honoured via the IOS_LEGACY=1 environment variable.
    #[arg(long, global = true)]
    legacy: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List connected iOS devices
    Devices,

    /// Print device information from lockdownd
    Info {
        #[arg(long)]
        udid: Option<String>,
    },

    /// List available services on the device
    Services {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Forward a local TCP port to a device service port
    Relay {
        /// Device service port to connect to
        port: u16,
        #[arg(long, default_value = "0")]
        listen: u16,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Watch for device attach/detach events
    Watch,

    /// Capture a screenshot (PNG)
    Screenshot {
        /// Output file path (use - for stdout)
        #[arg(default_value = "screenshot.png")]
        output: String,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Reboot the device
    Reboot {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Shut down the device
    Shutdown {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Device diagnostics (battery, full dump)
    Diagnostics {
        #[command(subcommand)]
        action: DiagnosticsAction,
    },

    /// Stream live syslog output from the device (Ctrl-C to stop)
    Syslog {
        /// Filter entries to those whose process name contains this string
        #[arg(long)]
        process: Option<String>,
        /// Filter entries to those whose text contains this string (case-insensitive)
        #[arg(long)]
        filter: Option<String>,
        /// Output newline-delimited JSON instead of raw log lines
        #[arg(long)]
        json: bool,
        /// Save output to a file instead of stdout
        #[arg(short, long)]
        output: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Crash report management
    Crash {
        #[command(subcommand)]
        action: CrashAction,
    },


    /// Darwin notification proxy
    Notification {
        #[command(subcommand)]
        action: NotificationAction,
    },

    /// List running processes (requires Developer Mode)
    Ps {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Simulate or clear device GPS location (requires Developer Mode)
    Location {
        #[command(subcommand)]
        action: LocationAction,
    },

    /// Stream structured os_log output (Ctrl-C to stop)
    Oslog {
        /// Filter by process name
        #[arg(long)]
        process: Option<String>,
        /// Minimum level: default, info, debug, error, fault [default: default]
        #[arg(long)]
        level: Option<String>,
        /// Output newline-delimited JSON
        #[arg(long)]
        json: bool,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Show device IP / network addresses
    Deviceip {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Pair with the device.  Without flags: shows Trust dialog on device.
    /// Supervised mode (no dialog): use --supervision-p12 or --supervision-cert+--supervision-key.
    Pair {
        /// P12/PFX file containing the supervision certificate and private key.
        #[arg(long)]
        supervision_p12: Option<String>,
        /// Password for the P12 file (default: empty string).
        #[arg(long)]
        supervision_password: Option<String>,
        /// Supervision certificate file (DER or PEM). Requires --supervision-key.
        #[arg(long)]
        supervision_cert: Option<String>,
        /// Supervision RSA private key file (PEM PKCS#8/PKCS#1 or DER). Requires --supervision-cert.
        #[arg(long)]
        supervision_key: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Remove the pairing record for this device
    Unpair {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Get or set the device name
    Devicename {
        /// New device name to set (omit to print current name)
        name: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Show iOS version and available connection paths
    Version {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Browse and transfer files on the device media partition (DCIM, Books, …)
    Afc {
        #[command(subcommand)]
        action: AfcAction,
    },

    /// App management (list, install, uninstall)
    Apps {
        #[command(subcommand)]
        action: AppsAction,
    },

    /// Get or set screen orientation (set requires pre-installed OrientationHelper XCTest)
    Orientation {
        #[command(subcommand)]
        action: OrientationAction,
    },

    /// Get or set device language and locale
    Lang {
        /// Set language (e.g. "en", "de", "zh-Hans")
        #[arg(long = "setlang")]
        set_lang: Option<String>,
        /// Set locale (e.g. "en_US", "de_DE")
        #[arg(long = "setlocale")]
        set_locale: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Get or set device timezone and clock
    Date {
        /// Set timezone (e.g. "America/New_York", "Europe/Berlin")
        #[arg(long = "settz")]
        timezone: Option<String>,
        /// Sync device clock to host time
        #[arg(long)]
        sync: bool,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Show RSD service catalogue via CDTunnel (iOS 17.4+)
    Rsd {
        #[arg(long)]
        udid: Option<String>,
    },

    /// Mount the personalized Developer Disk Image (unlocks Instruments / dtservicehub)
    Mounter {
        #[command(subcommand)]
        action: MounterAction,
    },

    /// Live performance monitoring (CPU, RAM per process) via Instruments sysmontap
    Perf {
        /// Output newline-delimited JSON instead of the live htop view
        #[arg(long)]
        json: bool,
        /// Sampling interval in milliseconds (default: 1000)
        #[arg(long, default_value = "1000")]
        interval: u64,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Run XCTest bundle (UI or unit tests) on iOS 17.4+
    Runtest {
        #[arg(long)]
        bundle_id: Option<String>,
        #[arg(long = "test-runner-bundle-id")]
        test_runner_bundle_id: String,
        #[arg(long = "xctest-config")]
        xctest_config: String,
        #[arg(long = "test-to-run")]
        tests_to_run: Vec<String>,
        #[arg(long = "test-to-skip")]
        tests_to_skip: Vec<String>,
        /// Run as a unit test (not a UI test)
        #[arg(long)]
        xctest: bool,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        udid: Option<String>,
    },

    /// Start WebDriverAgent on iOS 17.4+
    Runwda {
        #[arg(long = "bundleid", default_value = "com.facebook.WebDriverAgentRunner")]
        bundle_id: String,
        #[arg(long = "testrunnerbundleid", default_value = "com.facebook.WebDriverAgentRunner.xctrunner")]
        test_runner_bundle_id: String,
        #[arg(long = "xctestconfig", default_value = "WebDriverAgentRunner.xctest")]
        xctest_config: String,
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum CrashAction {
    /// List crash reports on the device
    Ls {
        #[arg(short, long)]
        long: bool,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Download a crash report
    Pull {
        /// Report filename (as shown by `crash ls`)
        name: String,
        /// Local destination path (defaults to the filename)
        local: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Delete a crash report from the device
    Rm {
        name: String,
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum LocationAction {
    /// Set a simulated GPS location
    Set {
        lat: f64,
        lon: f64,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Clear simulated location and restore real GPS
    Clear {
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum NotificationAction {
    /// Post a Darwin notification
    Post {
        /// Notification name (e.g. com.apple.springboard.hasBlankedScreen)
        name: String,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Observe Darwin notifications (Ctrl-C to stop)
    Observe {
        /// Notification name to watch (omit to observe all)
        name: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum DiagnosticsAction {
    /// Battery state — capacity, voltage, cycle count
    Battery {
        #[arg(long)]
        udid: Option<String>,
    },
    /// Full diagnostics dump (raw plist)
    All {
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum AfcAction {
    /// List directory contents
    Ls {
        /// Show size, type, and modification time
        #[arg(short, long)]
        long: bool,
        /// Path on the device (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Print metadata for a file or directory
    Stat {
        path: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Print device file-system info (model, free space)
    Info {
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Download a file or directory from the device
    Pull {
        /// Remote path on the device
        remote: String,
        /// Local destination path
        local: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Upload a file or directory to the device
    Push {
        /// Local source path
        local: String,
        /// Remote destination path on the device
        remote: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Remove a file or directory
    Rm {
        path: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Create a directory
    Mkdir {
        path: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Rename or move a file or directory
    Mv {
        from: String,
        to: String,
        /// Access an app's container instead of the media partition
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum MounterAction {
    /// Mount the personalized DDI (downloads automatically on first run)
    Mount {
        #[arg(long)]
        udid: Option<String>,
    },
    /// Check if the developer disk image is currently mounted
    Status {
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum AppsAction {
    /// List installed apps
    List {
        #[arg(long)]
        udid: Option<String>,
        #[arg(long)]
        system: bool,
        #[arg(long)]
        all: bool,
    },
    /// Install an IPA file
    Install {
        ipa: String,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Uninstall an app by bundle ID
    Uninstall {
        bundle_id: String,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Launch an app by bundle ID (iOS 17.4+)
    Launch {
        bundle_id: String,
        /// Kill any existing instance before launching
        #[arg(long)]
        terminate_existing: bool,
        #[arg(long)]
        udid: Option<String>,
    },
    /// Kill a process by PID (sends SIGKILL, iOS 17.4+)
    Kill {
        pid: i64,
        #[arg(long)]
        udid: Option<String>,
    },
}

#[derive(Subcommand)]
enum OrientationAction {
    /// Read current screen orientation from SpringBoard
    Get {
        #[arg(long)]
        udid: Option<String>,
    },
    /// Set screen orientation via a short-lived XCUITest (see `ios orientation set --help`)
    Set {
        /// Target orientation: portrait, portrait_upside_down, landscape_left, landscape_right
        direction: String,
        /// Bundle ID of the app under test (leave empty for unit tests)
        #[arg(long)]
        bundle_id: Option<String>,
        /// Test runner bundle ID
        #[arg(long, default_value = "it.luedeke.devicelink.orientationhelper.xctrunner")]
        runner_bundle_id: String,
        /// XCTest config name (default: ios-rs-helperUITests.xctest)
        #[arg(long, default_value = "ios-rs-helperUITests.xctest")]
        xctest_config: String,
        #[arg(long)]
        udid: Option<String>,
    },
}

fn main() -> Result<()> {
    install_crypto_provider();
    let cli  = Cli::parse();
    let mode = ConnectionMode::from_env().with_legacy_flag(cli.legacy);

    match cli.command {
        Cmd::Devices          => cmd::devices::run(),
        Cmd::Info   { udid }  => cmd::info::run(udid.as_deref(), mode),
        Cmd::Services { udid} => cmd::services::run(udid.as_deref(), mode),
        Cmd::Relay { port, listen, udid } => cmd::relay::run(udid.as_deref(), port, listen),
        Cmd::Watch            => cmd::watch::run(),
        Cmd::Screenshot { output, udid } =>
            cmd::screenshot::run(udid.as_deref(), mode, &output),
        Cmd::Reboot   { udid } => cmd::diagnostics::reboot(udid.as_deref(), mode),
        Cmd::Shutdown { udid } => cmd::diagnostics::shutdown(udid.as_deref(), mode),
        Cmd::Diagnostics { action } => match action {
            DiagnosticsAction::Battery { udid } => cmd::diagnostics::battery(udid.as_deref(), mode),
            DiagnosticsAction::All     { udid } => cmd::diagnostics::all(udid.as_deref(), mode),
        },
        Cmd::Syslog { process, filter, json, output, udid } =>
            cmd::syslog::run(udid.as_deref(), mode, process.as_deref(), filter.as_deref(), json, output.as_deref()),
        Cmd::Crash { action } => match action {
            CrashAction::Ls   { long, udid }       => cmd::crash::ls(udid.as_deref(), mode, long),
            CrashAction::Pull { name, local, udid } => cmd::crash::pull(udid.as_deref(), mode, &name, local.as_deref()),
            CrashAction::Rm   { name, udid }        => cmd::crash::rm(udid.as_deref(), mode, &name),
        },
        Cmd::Notification { action } => match action {
            NotificationAction::Post    { name, udid }        => cmd::notification::post(udid.as_deref(), mode, &name),
            NotificationAction::Observe { name, udid }        => cmd::notification::observe(udid.as_deref(), mode, name.as_deref()),
        },
        Cmd::Ps { udid }   => cmd::ps::run(udid.as_deref()),
        Cmd::Location { action } => match action {
            LocationAction::Set   { lat, lon, udid } => cmd::location::set(udid.as_deref(), lat, lon),
            LocationAction::Clear { udid }           => cmd::location::clear(udid.as_deref()),
        },
        Cmd::Oslog { process, level, json, udid } =>
            cmd::oslog::run(udid.as_deref(), process.as_deref(), level.as_deref(), json),
        Cmd::Deviceip { udid } => cmd::deviceip::run(udid.as_deref()),
        Cmd::Pair { udid, supervision_p12, supervision_password, supervision_cert, supervision_key } =>
            cmd::pair::pair(
                udid.as_deref(),
                supervision_cert.as_deref(),
                supervision_key.as_deref(),
                supervision_p12.as_deref(),
                supervision_password.as_deref(),
            ),
        Cmd::Unpair { udid } => cmd::pair::unpair(udid.as_deref()),
        Cmd::Devicename { name, udid } => {
            let mut session = cmd::open_session(udid.as_deref(), mode)?;
            match name {
                None => {
                    let v = session.lockdown().get_value(None, "DeviceName")
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    println!("{}", v.as_string().unwrap_or("(unknown)"));
                }
                Some(n) => {
                    session.lockdown().set_value(None, "DeviceName", plist::Value::String(n.clone()))
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    eprintln!("device name set to {n:?}");
                }
            }
            Ok(())
        }
        Cmd::Version { udid } => cmd::version::run(udid.as_deref()),
        Cmd::Orientation { action } => match action {
            OrientationAction::Get { udid } =>
                cmd::orientation::get(udid.as_deref(), mode),
            OrientationAction::Set { direction, bundle_id, runner_bundle_id, xctest_config, udid } =>
                cmd::orientation::set(
                    udid.as_deref(), &direction,
                    bundle_id.as_deref(), &runner_bundle_id, &xctest_config,
                ),
        },
        Cmd::Lang { set_lang, set_locale, udid } =>
            cmd::lang::run(udid.as_deref(), set_lang.as_deref(), set_locale.as_deref(), mode),
        Cmd::Date { timezone, sync, udid } =>
            cmd::timezone::run(udid.as_deref(), timezone.as_deref(), sync, mode),
        Cmd::Rsd { udid }     => cmd::rsd::run(udid.as_deref()),
        Cmd::Mounter { action } => match action {
            MounterAction::Mount  { udid } => cmd::mounter::mount(udid.as_deref()),
            MounterAction::Status { udid } => cmd::mounter::status(udid.as_deref()),
        },
        Cmd::Perf { json, interval, udid } =>
            cmd::perf::run(udid.as_deref(), json, interval),
        Cmd::Runtest { bundle_id, test_runner_bundle_id, xctest_config,
                       tests_to_run, tests_to_skip, xctest, env, udid } =>
            cmd::runtest::run_test(
                udid.as_deref(),
                bundle_id.as_deref().unwrap_or(""),
                &test_runner_bundle_id,
                &xctest_config,
                tests_to_run,
                tests_to_skip,
                xctest,
                env,
            ),
        Cmd::Runwda { bundle_id, test_runner_bundle_id, xctest_config, udid } =>
            cmd::runtest::run_wda(
                udid.as_deref(),
                &bundle_id,
                &test_runner_bundle_id,
                &xctest_config,
            ),
        Cmd::Afc { action } => match action {
            AfcAction::Ls   { long, path, app, udid } =>
                cmd::afc::ls(udid.as_deref(), mode, &path, long, app.as_deref()),
            AfcAction::Stat { path, app, udid } =>
                cmd::afc::stat(udid.as_deref(), mode, &path, app.as_deref()),
            AfcAction::Info { app, udid } =>
                cmd::afc::info(udid.as_deref(), mode, app.as_deref()),
            AfcAction::Pull { remote, local, app, udid } =>
                cmd::afc::pull(udid.as_deref(), mode, &remote, std::path::Path::new(&local), app.as_deref()),
            AfcAction::Push { local, remote, app, udid } =>
                cmd::afc::push(udid.as_deref(), mode, std::path::Path::new(&local), &remote, app.as_deref()),
            AfcAction::Rm   { path, app, udid } =>
                cmd::afc::rm(udid.as_deref(), mode, &path, app.as_deref()),
            AfcAction::Mkdir { path, app, udid } =>
                cmd::afc::mkdir(udid.as_deref(), mode, &path, app.as_deref()),
            AfcAction::Mv   { from, to, app, udid } =>
                cmd::afc::mv(udid.as_deref(), mode, &from, &to, app.as_deref()),
        },
        Cmd::Apps { action }  => match action {
            AppsAction::List { udid, system, all } =>
                cmd::apps::list::run(udid.as_deref(), system, all, mode),
            AppsAction::Install { ipa, udid } =>
                cmd::apps::install::run(udid.as_deref(), &ipa, mode),
            AppsAction::Uninstall { bundle_id, udid } =>
                cmd::apps::uninstall::run(udid.as_deref(), &bundle_id, mode),
            AppsAction::Launch { bundle_id, terminate_existing, udid } => {
                let mut session = cmd::open_session(udid.as_deref(), mode)?;
                let pid = cmd::apps::launch::run(&mut session, &bundle_id, terminate_existing)?;
                println!("launched {bundle_id} → pid {pid}");
                Ok(())
            }
            AppsAction::Kill { pid, udid } => {
                let mut session = cmd::open_session(udid.as_deref(), mode)?;
                cmd::apps::kill::run(&mut session, pid)?;
                println!("sent SIGKILL to pid {pid}");
                Ok(())
            }
        },
    }
}
