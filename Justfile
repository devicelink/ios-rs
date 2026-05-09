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
    cargo clippy -- -D warnings

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

# ── Combined ─────────────────────────────────────────────────────────────────

# Run everything CI runs (build, test, lint, readme check)
ci: build-all test-all lint readme-check
