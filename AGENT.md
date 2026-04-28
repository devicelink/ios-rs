# AGENT.md

Notes for AI agents and contributors working in this codebase.

## Build

```sh
cargo build            # all crates
cargo build -p ios     # CLI only
cargo test             # unit + integration tests (requires device for device.rs tests)
```

Integration tests that need a real device are gated behind the `device` feature or skipped if usbmuxd is unreachable.

## Environment

Connect via TCP proxy (required in containers):

```sh
export USBMUXD_SOCKET_ADDRESS=127.0.0.1:27015
```

Force legacy lockdownd path (skips CDTunnel/RSD):

```sh
export IOS_LEGACY=1
```

## Crate map

```
usbmux-proto   sans-IO codec — no I/O, pure state machine
usbmux         blocking client wrapping usbmux-proto
usbmux-sim     in-process simulator (used in tests)
lockdown       lockdownd session, pair records, AFC, installation_proxy
xpc-proto      XPC binary encode/decode
remotexpc      RemoteXPC over HTTP/2
rsd            Remote Service Discovery client
tunnel         CDTunnel + smoltcp stack + DeviceSession routing
ios            CLI binary
```

## Key types

- **`usbmux::Connection`** — opens usbmux socket, lists devices, opens tunnels
- **`lockdown::LockdownSession`** — paired lockdownd session with TLS; `start_service()` returns a port + SSL flag
- **`tunnel::DeviceSession`** — single entry point for consumers; auto-selects RSD or legacy path
- **`tunnel::SmoltcpTunnel`** — userspace IPv6 stack; `connect(addr, port)` returns a `UnixStream`
- **`rsd::RsdClient`** — parses the RSD Handshake; holds the service catalogue

## Connection flow (iOS 17.4+)

1. `usbmux::Connection::open()` → list devices, open TCP tunnel to lockdownd port 62078
2. `LockdownSession::open_paired()` → StartSession (TLS), read pair record from usbmuxd
3. `session.start_service("com.apple.internal.devicecompute.CoreDeviceProxy")` → port + SSL=true
4. Open second usbmux TCP tunnel to that port; wrap in rustls TLS 1.2 (mTLS with host cert)
5. `CdTunnelConn::handshake_params(&mut tls)` — send `clientHandshakeRequest` JSON, receive `serverHandshakeResponse` with IPv6 addresses and RSD port
6. Set 2ms read timeout on underlying TcpStream; pass TLS stream to `SmoltcpTunnel::new_stream()`
7. Unified poll thread: non-blocking reads (WouldBlock/TimedOut → no packet), drive smoltcp, write TX packets
8. `SmoltcpTunnel::connect(server_addr, rsd_port)` → TCP ESTABLISHED inside tunnel → `UnixStream` pair
9. `RsdClient::connect_stream(unix_stream)` → RemoteXPC H2 handshake → parse Handshake message (skip init frames, reassemble split H2 DATA frames)
10. `DeviceSession::connect_rsd_shim(name)` → RSDCheckin plist → drain ack + StartService → `MuxSocket::External`

## Framing reference

| Layer | Format |
|---|---|
| usbmux | 16-byte LE header (`length`, `version=1`, `type`, `tag`) + XML plist |
| lockdownd | 4-byte BE length + XML plist |
| CDTunnel handshake | `b"CDTunnel"` + u16-BE length + JSON |
| CDTunnel packets | raw IPv6 (no framing after handshake) |
| RemoteXPC / XPC | 24-byte XPC header (`WRAPPER_MAGIC`, flags, body_len, msg_id) + XPC binary payload |
| H2 frame | 9-byte header (3-byte length, type, flags, stream_id) + payload |
| RSD shim (`.shim.remote`) | 4-byte BE length + XML plist; RSDCheckin before first command |

## Known edge cases

**H2 frame reassembly** (`remotexpc/src/conn.rs`): The iOS RSD Handshake is ~17 KiB, which exceeds the H2 default max frame size. `receive()` accumulates DATA frame payloads in `recv_buf` until the XPC `body_len` is satisfied. `recv_buf` persists between calls to handle the case where one H2 frame contains multiple XPC messages.

**RSD init frames**: Before the Handshake, the device sends 1–3 XPC messages with no body (flags=`ALWAYS_SET`, body_len=0) and one with an empty dict (flags=0x1). `RsdClient::from_conn` loops past these until it finds a message with a `Properties` key.

**RSD shim startup**: After the `RSDCheckin` plist, the shim sends two control messages before the service is usable: a checkin acknowledgment (Request=`"RSDCheckin"`) and a `StartService` notification. `connect_rsd_shim` drains these, stopping when it sees `Request == "StartService"`.

**TLS stream in smoltcp**: `SmoltcpTunnel::new_stream()` is used when the CDTunnel runs over TLS (the common case). It uses a single unified I/O+poll thread instead of the separate reader+poll threads used for plain TCP, because a `StreamOwned<ClientConnection, TcpStream>` cannot be split for concurrent reads and writes. A 2ms read timeout on the underlying `TcpStream` keeps the loop responsive without busy-spinning.

**CDTunnel field names**: The iOS device sends camelCase JSON (`serverAddress`, `serverRSDPort`, `clientParameters`), not PascalCase. The `ServerHandshake` struct uses `#[serde(rename_all = "camelCase")]`.

## Adding a new RSD shim service

Most legacy lockdown services are proxied as `com.apple.X.shim.remote` in the RSD catalogue. To use one:

```rust
// In DeviceSession context (RSD path active):
let stream = session.connect_rsd_shim("com.apple.mobile.my_service.shim.remote")?;
// stream is ready for the native 4-byte-plist protocol
```

Services with `uses_remote_xpc: true` in the catalogue speak RemoteXPC instead of plain plist and do not need `RSDCheckin`.
