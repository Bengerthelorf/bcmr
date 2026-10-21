---
title: scp Compatibility
section: guide
order: 9
---

This describes development source after the published 0.7.0 package, compared
with the [OpenSSH scp manual](https://man.openbsd.org/scp).

| Capability | bcmr behavior |
|---|---|
| Upload/download, multiple sources, recursive directories | Available; local and remote destination rules match. |
| Source patterns | `*` locally and remotely; quote remote patterns in zsh. |
| Temporary SSH port/key/config/jump | `--ssh-port`, repeated `--ssh-identity`, `--ssh-config`, `--ssh-jump`; also inherited SSH configuration. `-P` remains concurrency and `--config` remains bcmr config. |
| URI / IPv6 / endpoint port | `scp://user@host:2222/absolute/path`, bracketed IPv6 and percent-escaped UTF-8; endpoint port overrides `--ssh-port`. |
| Aggregate bandwidth cap | `--bandwidth-limit 10MiB/s`, shared across workers and fan-out. |
| Mixed local/remote sources | Supported with one destination plan and overwrite approval. |
| Remote-to-remote | Bounded streams through this client; source and destination credentials stay local. |
| SFTP-only account | Real SFTP v3 backend, detected automatically; force with `--remote-backend=sftp`. No remote exec needed. File publication requires `hardlink@openssh.com`. |
| Password/key-passphrase auth | Native OpenSSH prompts in interactive legacy/SFTP connections; unattended connections use batch authentication. Serve can fall back to legacy when batch auth is unavailable. |
| Metadata and links | `-p`, recursive link following/cycle detection, `--no-deref`; moves preserve link objects. Backend precision/capability limits apply. |
| Verification/resume/dry-run/fan-out/JSON | Additional bcmr workflows; unsupported combinations report an error. |

The existing SSH fallback runs remote shell tools; it is distinct from both the
SFTP backend and the historical SCP wire protocol. Incompatible serve versions
can trigger SSH fallback independently of patterns.

Default no-clobber, explicit overwrite approval, and rejecting remote `..` or a
bare filesystem root are intentional differences. Source hardlink topology and
arbitrary ACL/xattr replication are outside portable `-p` semantics.

See [SSH options](./ssh-options.md), [transfer backends](./transfer-backends.md)
and [local/remote behavior](./local-remote-semantics.md) for examples and exact
limits. These source changes do not change the contents of an existing 0.7.0
Homebrew installation.
