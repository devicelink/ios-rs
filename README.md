# ios-rs

A Rust crate for interacting with iOS devices over USB via usbmuxd. Implements the full iOS 17+ connection stack from scratch — usbmux framing, lockdownd sessions, CDTunnel/RSD, RemoteXPC over HTTP/2, and a userspace IPv6 stack — without any Apple SDK dependency.

Tested against an iPhone SE running iOS 18.7.1.

## Connection paths

| iOS version | Path |
|---|---|
| < 17 | usbmux → lockdownd |
| 17.0–17.3 | usbmux → lockdownd (RSD over USB-Ethernet not yet implemented) |
| 17.4+ | usbmux → lockdownd → **CoreDeviceProxy** (TLS) → **CDTunnel** → smoltcp IPv6 → **RSD** → shim services |

The modern path is selected automatically. Use `--legacy` or `IOS_LEGACY=1` to force the lockdownd path on any version.

## Features

The crate is organised into feature layers. Enable only what you need:

| Feature | Enables | Pulls in |
|---|---|---|
| `lockdown` | lockdownd sessions: TLS, StartSession, StartService, pair records, AFC, installation_proxy | rustls |
| `tunnel` | CDTunnel handshake, smoltcp userspace IPv6 stack, `DeviceSession` routing | `lockdown` + smoltcp + crypto |
| `xctest` | XCTest / XCUITest runner | `tunnel` |
| `cli` | `ios` CLI binary | `tunnel` + `xctest` + clap + ureq |
| `sim` | In-process usbmux simulator for integration tests | — |

`default = ["tunnel", "cli"]`. The usbmux codec and XPC/RemoteXPC layers are always compiled unconditionally as they carry no heavy dependencies.

## Installation

Pre-built binaries are published on every merge to main.

**macOS — Apple Silicon**
```sh
curl -fsSL https://github.com/devicelink/ios-rs/releases/latest/download/ios-aarch64-apple-darwin.tar.gz | tar xz -C /usr/local/bin
```

**macOS — Intel**
```sh
curl -fsSL https://github.com/devicelink/ios-rs/releases/latest/download/ios-x86_64-apple-darwin.tar.gz | tar xz -C /usr/local/bin
```

**Linux**
```sh
curl -fsSL https://github.com/devicelink/ios-rs/releases/latest/download/ios-x86_64-unknown-linux-gnu.tar.gz | tar xz -C /usr/local/bin
```

**Windows** (PowerShell)
```powershell
irm https://github.com/devicelink/ios-rs/releases/latest/download/ios-x86_64-pc-windows-msvc.zip -OutFile ios.zip
Expand-Archive ios.zip .
Move-Item ios.exe "$env:LOCALAPPDATA\Microsoft\WindowsApps\ios.exe"
```

Or build from source:
```sh
cargo install --git https://github.com/devicelink/ios-rs --features cli
```

## CLI

```sh
cargo build --bin ios --features cli
```

<!-- help:start -->
### `ios`

```
Interact with iOS devices via usbmuxd

Usage: ios [OPTIONS] <COMMAND>

Commands:
  devices      List connected iOS devices
  info         Print device information from lockdownd
  services     List available services on the device
  relay        Forward a local TCP port to a device service port
  watch        Watch for device attach/detach events
  version      Show iOS version and available connection paths
  apps         App management (list, install, uninstall)
  orientation  Get or set screen orientation (set requires pre-installed OrientationHelper XCTest)
  lang         Get or set device language and locale
  date         Get or set device timezone and clock
  rsd          Show RSD service catalogue via CDTunnel (iOS 17.4+)
  mounter      Mount the personalized Developer Disk Image (unlocks Instruments / dtservicehub)
  perf         Live performance monitoring (CPU, RAM per process) via Instruments sysmontap
  runtest      Run XCTest bundle (UI or unit tests) on iOS 17.4+
  runwda       Start WebDriverAgent on iOS 17.4+
  help         Print this message or the help of the given subcommand(s)
```

### `ios devices`

```
List connected iOS devices

Usage: ios devices [OPTIONS]
```

### `ios info`

```
Print device information from lockdownd

Usage: ios info [OPTIONS]
```

### `ios services`

```
List available services on the device

Usage: ios services [OPTIONS]
```

### `ios relay`

```
Forward a local TCP port to a device service port

Usage: ios relay [OPTIONS] <PORT>

Arguments:
  <PORT>  Device service port to connect to

Options:
      --listen <LISTEN>  [default: 0]
```

### `ios watch`

```
Watch for device attach/detach events

Usage: ios watch [OPTIONS]
```

### `ios version`

```
Show iOS version and available connection paths

Usage: ios version [OPTIONS]
```

### `ios apps`

```
App management (list, install, uninstall)

Usage: ios apps [OPTIONS] <COMMAND>

Commands:
  list       List installed apps
  install    Install an IPA file
  uninstall  Uninstall an app by bundle ID
  help       Print this message or the help of the given subcommand(s)
```

### `ios orientation`

```
Get or set screen orientation (set requires pre-installed OrientationHelper XCTest)

Usage: ios orientation [OPTIONS] <COMMAND>

Commands:
  get   Read current screen orientation from SpringBoard
  set   Set screen orientation via a short-lived XCUITest (see `ios orientation set --help`)
  help  Print this message or the help of the given subcommand(s)
```

### `ios lang`

```
Get or set device language and locale

Usage: ios lang [OPTIONS]

Options:
      --setlang <SET_LANG>      Set language (e.g. "en", "de", "zh-Hans")
      --setlocale <SET_LOCALE>  Set locale (e.g. "en_US", "de_DE")
```

### `ios date`

```
Get or set device timezone and clock

Usage: ios date [OPTIONS]

Options:
      --settz <TIMEZONE>  Set timezone (e.g. "America/New_York", "Europe/Berlin")
      --sync              Sync device clock to host time
```

### `ios rsd`

```
Show RSD service catalogue via CDTunnel (iOS 17.4+)

Usage: ios rsd [OPTIONS]
```

### `ios mounter`

```
Mount the personalized Developer Disk Image (unlocks Instruments / dtservicehub)

Usage: ios mounter [OPTIONS] <COMMAND>

Commands:
  mount   Mount the personalized DDI (downloads automatically on first run)
  status  Check if the developer disk image is currently mounted
  help    Print this message or the help of the given subcommand(s)
```

### `ios perf`

```
Live performance monitoring (CPU, RAM per process) via Instruments sysmontap

Usage: ios perf [OPTIONS]

Options:
      --json                 Output newline-delimited JSON instead of the live htop view
      --interval <INTERVAL>  Sampling interval in milliseconds (default: 1000) [default: 1000]
```

### `ios runtest`

```
Run XCTest bundle (UI or unit tests) on iOS 17.4+

Usage: ios runtest [OPTIONS] --test-runner-bundle-id <TEST_RUNNER_BUNDLE_ID> --xctest-config <XCTEST_CONFIG>

Options:
      --bundle-id <BUNDLE_ID>
          
      --test-runner-bundle-id <TEST_RUNNER_BUNDLE_ID>
          
      --xctest-config <XCTEST_CONFIG>
          
      --test-to-run <TESTS_TO_RUN>
          
      --test-to-skip <TESTS_TO_SKIP>
          
      --xctest
          Run as a unit test (not a UI test)
      --env <ENV>
          
```

### `ios runwda`

```
Start WebDriverAgent on iOS 17.4+

Usage: ios runwda [OPTIONS]

Options:
      --bundleid <BUNDLE_ID>
          [default: com.facebook.WebDriverAgentRunner]
      --testrunnerbundleid <TEST_RUNNER_BUNDLE_ID>
          [default: com.facebook.WebDriverAgentRunner.xctrunner]
      --xctestconfig <XCTEST_CONFIG>
          [default: WebDriverAgentRunner.xctest]
```

<!-- help:end -->

### Examples

```sh
# First connected device
ios devices
ios apps list

# Specific device
ios --udid 00008030-000E04D62E8B802E apps list

# Force legacy lockdownd path
ios --legacy apps list

# Show RSD service catalogue (iOS 17.4+)
ios rsd

# Connect via TCP proxy (e.g. inside Docker)
USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015 ios devices
```

## Protocol stack (iOS 17.4+)

```
┌─────────────────────────────────────────────────────────┐
│  ios CLI / library consumer                             │
├─────────────────────────────────────────────────────────┤
│  DeviceSession  (tunnel crate)                          │
│    ├─ RSD shim services  (4-byte plist, RSDCheckin)     │
│    └─ legacy lockdownd   (4-byte plist, TLS)            │
├─────────────────────────────────────────────────────────┤
│  smoltcp  — userspace IPv6/TCP stack                    │
├─────────────────────────────────────────────────────────┤
│  CDTunnel  — JSON handshake over TLS (go-ios approach)  │
│    b"CDTunnel" + u16-BE length + JSON body              │
├─────────────────────────────────────────────────────────┤
│  CoreDeviceProxy — lockdownd service (TLS 1.2 + mTLS)  │
├─────────────────────────────────────────────────────────┤
│  usbmux — TCP tunnel over USB                           │
└─────────────────────────────────────────────────────────┘
```

The CDTunnel handshake (`clientHandshakeRequest` → `serverHandshakeResponse`) gives us a private IPv6 subnet. The smoltcp stack runs entirely in userspace — no TUN interface, no root.

## RSD shim services

iOS 17.4+ exposes legacy lockdown services as `.shim.remote` entries in the RSD catalogue. Connecting to them requires an `RSDCheckin` plist before speaking the native protocol:

```rust
let stream = session.connect_rsd_shim("com.apple.mobile.installation_proxy.shim.remote")?;
let proxy  = InstallationProxy::from_stream(stream);
let apps   = proxy.list_apps(AppType::User)?;
```

## Environment variables

| Variable | Effect |
|---|---|
| `USBMUXD_SOCKET_ADDRESS=host:port` | Connect to usbmuxd via TCP (Docker, socat, etc.) |
| `IOS_LEGACY=1` | Force legacy lockdownd path, skip CDTunnel |

## Requirements

- Rust 1.75+
- usbmuxd running locally (macOS: built-in via iTunes/Finder; Linux: `usbmuxd` package)
- For TCP proxy in containers: `socat TCP-LISTEN:27015,fork UNIX-CONNECT:/var/run/usbmuxd`
