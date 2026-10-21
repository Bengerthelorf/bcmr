---
title: scp Compatibility
section: guide
order: 9
---

This audit describes development source as of 2026-10-21. Source-star expansion
and the multi-host download correction postdate the published 0.7.0 package.
The comparison uses the [OpenSSH scp manual](https://man.openbsd.org/scp).

| Capability | bcmr status |
|---|---|
| Upload, download, multiple sources, recursive directories | Available. Multiple downloads require an existing directory or an explicit trailing `/`. Different source hosts use the host-aware legacy SSH transport. |
| Source patterns | `*` is available locally and remotely. Other glob operators are not implemented. |
| SSH aliases, port, key, jump host, address family, cipher | Available through normal SSH configuration. Dedicated per-command CLI overrides are absent. `--config` configures bcmr, and `-P` sets transfer parallelism. |
| Password/key-passphrase authentication | Interactive legacy SSH can use native prompts. Serve connections use batch authentication and may fall back when it is unavailable. |
| IPv6 | Bracketed host syntax is available, such as `[::1]:file`. |
| File metadata and compression | Available with `-p`, wire compression options and SSH compression configuration. Directory metadata preservation is incomplete. |
| Remote-to-remote copying | Missing; currently requires a local intermediate. |
| Restricted SFTP-only accounts | Missing. Both bcmr serve and its legacy fallback require remote command execution. |
| `scp://` URI / inline port | Missing. Use `[user@]host:path` and configure the port in SSH configuration. |
| Bandwidth cap | Missing; parallelism controls concurrency, not bandwidth. |
| Recursive symbolic-link traversal | Does not fully match scp's following of directory links. Remote copying of the links themselves is also unsupported. |
| Mixed local/remote sources to a local destination | Rejected explicitly before transfer; use separate commands. |

The legacy fallback uses system SSH plus remote shell commands (`cat`, `stat`,
`find`); it is not the legacy SCP wire protocol or an SFTP client. An incompatible
serve protocol triggers fallback independently of source pattern matching.

Intentional differences include no-clobber behavior, explicit overwrite
confirmation, and refusing remote `..` or bare filesystem-root paths. These
are not absent scp capabilities. Verification, resume, dry-run, fan-out and JSON
events provide additional bcmr workflows.
