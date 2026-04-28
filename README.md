# ios-rs

A Rust workspace for interacting with iOS devices over USB via usbmuxd. Implements the full iOS 17+ connection stack from scratch — usbmux framing, lockdownd sessions, CDTunnel/RSD, RemoteXPC over HTTP/2, and a userspace IPv6 stack — without any Apple SDK dependency.

Tested against an iPhone SE running iOS 18.7.1.

## Connection paths

| iOS version | Path |
|---|---|
| < 17 | usbmux → lockdownd |
| 17.0–17.3 | usbmux → lockdownd (RSD over USB-Ethernet not yet implemented) |
| 17.4+ | usbmux → lockdownd → **CoreDeviceProxy** (TLS) → **CDTunnel** → smoltcp IPv6 → **RSD** → shim services |

The modern path is selected automatically. Use `--legacy` or `IOS_LEGACY=1` to force the lockdownd path on any version.

## Crates

| Crate | Purpose |
|---|---|
| `usbmux-proto` | Sans-IO usbmux codec (ListDevices, Connect, Listen) |
| `usbmux` | Blocking usbmux client; Unix socket or TCP proxy |
| `usbmux-sim` | In-process usbmux simulator for integration tests |
| `lockdown` | lockdownd sessions: TLS, StartSession, StartService, pair records, AFC, installation_proxy |
| `xpc-proto` | XPC binary value codec (encode + decode) |
| `remotexpc` | RemoteXPC over HTTP/2: handshake, send/receive XPC messages |
| `rsd` | Remote Service Discovery client: parses the device's service catalogue |
| `tunnel` | CDTunnel handshake, smoltcp userspace IPv6 stack, `DeviceSession` routing |
| `ios` | CLI binary |

## CLI

```
cargo build -p ios
```

```
ios [--legacy] <command>

Commands:
  devices                    List connected iOS devices
  info       [--udid <id>]   Device info from lockdownd
  version    [--udid <id>]   iOS version and available connection paths
  services   [--udid <id>]   List lockdownd services
  rsd        [--udid <id>]   RSD service catalogue via CDTunnel (iOS 17.4+)
  apps list  [--udid <id>]   List installed user apps
  apps list  --system        List system apps
  apps list  --all           List all apps
  apps install <ipa>         Install an IPA file
  apps uninstall <bundle-id> Uninstall an app
  relay <port> [--listen <port>]  TCP port forward to a device service
  watch                      Print device attach/detach events
```

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
