mod cmd;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tunnel::ConnectionMode;

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

    /// Show RSD service catalogue via CDTunnel (iOS 17.4+)
    Rsd {
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
        Cmd::Rsd { udid }     => cmd::rsd::run(udid.as_deref()),
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
