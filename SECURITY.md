# Security Policy

## Supported versions

Only the latest release on `main` receives security fixes.

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Report privately via [GitHub's private vulnerability reporting](https://github.com/devicelink/ios-rs/security/advisories/new).

Include:
- A description of the vulnerability and its potential impact
- Steps to reproduce
- Any suggested mitigations if known

You will receive a response within 72 hours. We will coordinate a fix and disclosure timeline with you before any public announcement.

## Dependency vulnerabilities

We run [`cargo audit`](https://rustsec.org/) in CI against the [RustSec Advisory Database](https://rustsec.org/advisories/).
If you discover a vulnerability in one of our dependencies, please report it to the upstream crate maintainer and to the [RustSec team](https://rustsec.org/contributing.html).
