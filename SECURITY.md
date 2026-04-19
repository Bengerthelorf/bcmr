# Security Policy

## Supported versions

Only the latest tagged release receives security fixes. Older releases are not backported.

## Reporting a vulnerability

**Do not file public issues for security bugs.** Use GitHub's private security advisory form:

<https://github.com/Bengerthelorf/bcmr/security/advisories/new>

If that's unavailable, email the repository owner (see the `authors` field in `Cargo.toml`).

## Scope

Particularly interested in reports touching:

- The direct-TCP data plane (AES-256-GCM via `ring`): nonce misuse, key reuse, framing desync
- Rendezvous authentication (`AuthChallenge` / `AuthHello`, blake3 keyed MAC)
- Path traversal in `bcmr serve` (jail escape under `--root`)
- Session file format (`src/core/session.rs`): tampering that leads to data corruption on resume
- CAS (`src/core/cas.rs`): multi-user isolation, symlink / TOCTOU

Out of scope: denial-of-service that requires physical/network access to the user's own machine (this is a CLI file-copy tool, not a network service).

## Response

Best-effort. This is an individual project; there is no SLA. A typical acknowledgement within a week, fix within two weeks for confirmed high-severity issues.
