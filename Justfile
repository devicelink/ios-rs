set shell := ["bash", "-euo", "pipefail", "-c"]
export PATH := env_var("HOME") + "/.cargo/bin:" + env_var("PATH")

# List available recipes
default:
    @just --list

# ── Build ────────────────────────────────────────────────────────────────────

# Build the ios CLI (debug)
build:
    cargo build --bin ios --features cli

# Build the ios CLI (release)
build-release:
    cargo build --release --bin ios --features cli

# Build everything (all features)
build-all:
    cargo build

# ── Test ─────────────────────────────────────────────────────────────────────

# Run library unit tests
test:
    cargo test --lib

# Run usbmux integration tests (simulator, no device needed)
test-integration:
    cargo test --test usbmux --features sim

# Run all tests
test-all: test test-integration

# ── Lint & format ────────────────────────────────────────────────────────────

# Run clippy
clippy:
    RUSTFLAGS="-D warnings" cargo clippy --all-targets --all-features

# Check formatting
fmt-check:
    cargo fmt -- --check

# Apply formatting
fmt:
    cargo fmt

# Run all lints (clippy + fmt check)
lint: clippy fmt-check

# ── Docs ─────────────────────────────────────────────────────────────────────

# Regenerate README help section from live binary output
readme:
    ./scripts/update-readme-help.sh

# Check README help section is up to date (used in CI)
readme-check:
    ./scripts/update-readme-help.sh
    git diff --exit-code README.md || { \
        echo ""; \
        echo "README help text is out of date. Run: just readme"; \
        exit 1; \
    }

# ── Install ──────────────────────────────────────────────────────────────────

PLIST := env_var("HOME") + "/Library/LaunchAgents/io.devicelink.ios-rsd.plist"
INSTALL_PATH := env_var("HOME") + "/.cargo/bin/ios"

# Install ios to ~/.cargo/bin
install:
    cargo install --path . --bin ios --features cli --locked
    @echo "Installed to {{ INSTALL_PATH }}"

# Register (or reload) the tunnel daemon LaunchAgent
daemon-load:
    @mkdir -p "$(dirname "{{ PLIST }}")"
    @printf '%s\n' \
        '<?xml version="1.0" encoding="UTF-8"?>' \
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
        '<plist version="1.0"><dict>' \
        '  <key>Label</key><string>io.devicelink.ios-rsd</string>' \
        '  <key>ProgramArguments</key><array>' \
        "  <string>{{ INSTALL_PATH }}</string>" \
        '  <string>tunnel</string><string>daemon</string>' \
        '  </array>' \
        '  <key>RunAtLoad</key><true/>' \
        '  <key>KeepAlive</key><true/>' \
        '  <key>StandardErrorPath</key><string>/tmp/ios-rsd.log</string>' \
        '</dict></plist>' \
        > "{{ PLIST }}"
    -launchctl unload "{{ PLIST }}" 2>/dev/null
    launchctl load "{{ PLIST }}"
    @echo "Daemon registered and started"

# Unload the tunnel daemon LaunchAgent
daemon-unload:
    launchctl unload "{{ PLIST }}"
    @echo "Daemon stopped and unregistered"

# ── Combined ─────────────────────────────────────────────────────────────────

# Run everything CI runs (build, test, lint, readme check)
ci: build-all test-all lint readme-check
