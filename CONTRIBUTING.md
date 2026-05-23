# Contributing

## Development setup

Requirements: Rust 1.75+ and [just](https://github.com/casey/just).

```sh
cargo build              # library only
just build               # ios CLI (debug)
just test-all            # unit + integration tests (no device needed)
just lint                # clippy + rustfmt check
just ci                  # everything CI runs locally
```

The integration tests use an in-process usbmux simulator (`--features sim`) so no physical device is required.

## Pull requests

1. Fork the repo and create a branch: `git checkout -b feat/my-feature`
2. Make your changes with tests where the behaviour is non-trivial
3. Ensure `just ci` passes before opening the PR
4. Open a PR against `main` and fill out the template

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add --timeout flag to ios relay
fix: handle empty plist response in lockdownd StartSession
docs: document CDTunnel handshake in README
chore: bump smoltcp to 0.12
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`.

A breaking change gets a `!` suffix and a `BREAKING CHANGE:` footer:

```
feat!: rename DeviceSession::connect to DeviceSession::open

BREAKING CHANGE: connect() is now open() to align with the rest of the API
```

## Code style

- `cargo fmt` defaults — no overrides
- `cargo clippy -- -D warnings` must be clean; CI enforces this
- Public API items require a doc comment
- No `unwrap()` / `expect()` in library code; use `?` with typed errors

## Release process

Releases are fully automated via [release-plz](https://release-plz.dev).

1. Merge conventional commits to `main` as normal.
2. release-plz opens (or updates) a **Release PR** that bumps the version in `Cargo.toml` and summarises the changes. The version bump is derived from the commits: `fix`/`perf` → patch, `feat` → minor, `BREAKING CHANGE` → major.
3. When you're ready to ship, merge the Release PR.
4. release-plz creates the version tag (e.g. `v0.2.0`), which triggers the release workflow to build cross-platform binaries and publish the GitHub Release.

**One-time setup (repo maintainer only):** create a fine-grained GitHub PAT with *Contents* and *Pull requests* read/write permissions on this repo, and store it as the `RELEASE_PLZ_TOKEN` repository secret.

## Reporting bugs

Open a [GitHub issue](https://github.com/devicelink/ios-rs/issues/new/choose) using the bug report template.
Include your iOS version, device model, and the output of `ios version`.
