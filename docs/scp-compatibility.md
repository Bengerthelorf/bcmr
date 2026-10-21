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
| File metadata and compression | Available with `-p`, wire compression options and SSH compression configuration. Root, nested and empty directory attributes are finalized after content. |
| Remote-to-remote copying | Missing; currently requires a local intermediate. |
| Restricted SFTP-only accounts | Missing. Both bcmr serve and its legacy fallback require remote command execution. |
| `scp://` URI / inline port | Missing. Use `[user@]host:path` and configure the port in SSH configuration. |
| Bandwidth cap | Missing; parallelism controls concurrency, not bandwidth. |
| Recursive symbolic-link traversal | Copy follows directory links and detects cycles. Local and remote `--no-deref` preserve links; move preserves link identity. |
| Mixed local/remote sources to a local destination | Rejected explicitly before transfer; use separate commands. |

The legacy fallback uses system SSH plus remote shell commands (`cat`, `stat`,
`find`); it is not the legacy SCP wire protocol or an SFTP client. An incompatible
serve protocol triggers fallback independently of source pattern matching.

Intentional differences include no-clobber behavior, explicit overwrite
confirmation, and refusing remote `..` or bare filesystem-root paths. These
are not absent scp capabilities. Verification, resume, dry-run, fan-out and JSON
events provide additional bcmr workflows.

## Recommended next capabilities

Temporary SSH overrides are useful for one-off ports, identities and jump hosts. Suggested long options are `--ssh-port`, repeatable `--ssh-identity`, `--ssh-config` and `--ssh-jump`; retain `-P` for transfer concurrency. These are design proposals, not currently available flags. Use one typed connection configuration initialized before source-star expansion, shared by serve, legacy I/O, preflight, cleanup, deploy, doctor, completion and background tasks. Connection overrides must change the multiplex identity or disable reuse. An identity file is additive according to [OpenSSH authentication configuration](https://man.openbsd.org/ssh_config.5); it is not a promise of exclusive key use. Arbitrary command-fragment options are unnecessary.

| Capability | Recommendation | Main requirement |
|---|---|---|
| Temporary SSH options | High; next coherent feature | Shared connection policy, precedence and multiplex identity. |
| Aggregate bandwidth cap | High for shared links | One aggregate limiter across workers, explicit units and accounting. |
| `scp://` and URI endpoint ports | Medium; compatibility convenience | Structured endpoint parsing and pool identity, including IPv6 and escaping. |
| Mixed local/remote sources | Medium; convenience | Composite planning, conflict detection, progress and safe cleanup. |
| Remote-to-remote relay | Useful independent feature | Prefer bounded client-mediated streaming, two-sided cancellation, verification and staged publication; no source-side destination credentials. |
| SFTP-only backend | Necessary for broad scp replacement | A real backend for inventory, metadata and data I/O; remote exec flags cannot substitute for it. |

Correctness fixes come first, followed by shared SSH options and bandwidth control. Relay and SFTP should be built as separate backend work according to deployment needs. Reproducing every historical scp short option is unnecessary.
