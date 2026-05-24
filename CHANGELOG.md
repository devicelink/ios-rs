# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/devicelink/ios-rs/releases/tag/v0.1.0) - 2026-05-24

### Added

- add RSD tunnel daemon and global JSON/text output mode

### Fixed

- align just lint with CI (--all-targets --all-features) and apply rustfmt
- resolve clippy warnings caught by CI (RUSTFLAGS=-D warnings)
- screenshot arg conflict and daemon liveness-probe EOF log
- resolve all clippy warnings now enforced by RUSTFLAGS=-D warnings
- resolve CI failures from RUSTFLAGS=-D warnings and cargo-audit
- malformed X.509 extensions in pairing certs broke TLS on iOS 18
- fix AFC integration tests: install rustls provider + document rename behaviour
- gate connect_stream on #[cfg(unix)] for wasm32-wasip2

### Other

- add lint check to pre-commit hook
- add open source project files and improve CI
- add --supervision-p12 to ios pair — accept P12 files directly
- add ios pair --supervision-cert/--supervision-key for supervised pairing
- add ios pair/unpair — full lockdownd pairing with RSA cert generation
- add ios apps launch/kill via coredevice.appservice + fix XPC date type
- add ios ps, location, oslog, deviceip commands
- remove pcap command — DDI required on iOS 17.4+ and not mountable via TSS
- remove incorrect StartCapture activation, document DDI requirement
- implement screenshot via dtservicehub + fix DTX fragmentation + fix smoltcp pipe backpressure
- implement screenshot via dtservicehub on iOS 17.4+
- add crash reports, PCAP, notifications, device name, syslog --output
- add screenshot, reboot, shutdown, and diagnostics commands
- implement live syslog streaming
- add per-app container file access via house_arrest
- implement AFC file system support
- add TODO.md with feature gap vs pymobiledevice3 and go-ios
- add tooling, release pipeline, and mounter/perf commands
- implement orientation set via XCUITest
- debug DTX + fix AFC opcodes; add DTX capability handshake
- add orientation-helper Xcode project + Fastlane pipeline
- add orientation get/set; SpringBoardClient
- add lang and date commands; add LockdownSession::set_value
- implement runtest / runwda for iOS 17.4+ via DTX + CoreDevice
- add cli feature to gate clap/comfy-table/anyhow
- flatten to single crate (no workspace)
- consolidate 9 crates into one library crate with features
- add CI workflow; fix all clippy warnings
- add README and AGENT.md
- iOS device management library and CLI in Rust
