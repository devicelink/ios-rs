mod cmd;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ios_rs::tunnel::ConnectionMode;

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

    /// Show iOS version and available connection paths
    Version {
        #[arg(long)]
        udid: Option<String>,
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
    let cli  = Cli::parse();
    let mode = ConnectionMode::from_env().with_legacy_flag(cli.legacy);

    match cli.command {
        Cmd::Devices          => cmd::devices::run(),
        Cmd::Info   { udid }  => cmd::info::run(udid.as_deref(), mode),
        Cmd::Services { udid} => cmd::services::run(udid.as_deref(), mode),
        Cmd::Relay { port, listen, udid } => cmd::relay::run(udid.as_deref(), port, listen),
        Cmd::Watch            => cmd::watch::run(),
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
        Cmd::Apps { action }  => match action {
            AppsAction::List { udid, system, all } =>
                cmd::apps::list::run(udid.as_deref(), system, all, mode),
            AppsAction::Install { ipa, udid } =>
                cmd::apps::install::run(udid.as_deref(), &ipa, mode),
            AppsAction::Uninstall { bundle_id, udid } =>
                cmd::apps::uninstall::run(udid.as_deref(), &bundle_id, mode),
        },
    }
}
