---
title: SSH Options and URI Paths
section: guide
order: 6
---

Use temporary SSH connection options before or after the subcommand:

```bash
bcmr --ssh-port 2222 copy report.pdf alice@storage:/srv/backups/
bcmr copy --ssh-identity ~/.ssh/work_ed25519 --ssh-jump bastion report.pdf storage:backup/
bcmr --ssh-config ./ssh.conf doctor storage
```

| Option | Effect for this invocation |
|---|---|
| `--ssh-port PORT` | Destination port, from 1 to 65535. |
| `--ssh-identity PATH` | Offer this identity; repeat to supply several files. |
| `--ssh-config PATH` | Read this OpenSSH configuration file. |
| `--ssh-jump HOST` | Use OpenSSH ProxyJump; a jump chain or `user@host:port` is accepted. |

These options apply to source expansion, transfers, checks, remote inspection,
move cleanup, `doctor`, and `deploy`, including serve/direct connection setup
and the SFTP backend. They do not change saved configuration. SSH aliases and
authentication otherwise follow OpenSSH configuration.

`--ssh-identity` adds identity arguments; it does not enable `IdentitiesOnly`.
Other configured keys and agent identities can still be offered. To restrict
authentication, put `IdentitiesOnly yes` and the intended `IdentityFile` in an
SSH configuration; use `IdentityAgent none` there if the agent must be disabled.

`-P` remains transfer concurrency. `--config` selects **bcmr** configuration;
`--ssh-config` selects **OpenSSH** configuration. For example:

```bash
bcmr --config ./bcmr.toml --ssh-config ./ssh.conf copy -P 4 report.pdf storage:backup/
```

## URI endpoints and port precedence

The URI form is `scp://[user@]host[:port]/absolute/path`. Its path is absolute;
use `storage:backup/` for a path relative to the remote login directory.
Existing `storage:/srv/backups/` and `storage://srv/backups/` spellings remain
available.

```bash
# This endpoint uses 2200, overriding the invocation's 2222 for that endpoint.
bcmr --ssh-port 2222 copy report.pdf 'scp://alice@storage:2200/srv/backups/'

# Bracket IPv6 addresses; quote URI paths for the shell.
bcmr copy 'scp://alice@[2001:db8::1]:2222/srv/data/report%20Q4.csv' ./downloads/

# Decode percent escapes as UTF-8: a space, a literal %, and a Unicode name.
bcmr copy 'scp://storage/srv/data/%E9%9B%AA%20%23100%25.txt' ./downloads/
```

Each explicit URI port overrides `--ssh-port` for its own endpoint. Endpoints
without a port use `--ssh-port`, then normal OpenSSH port configuration.
Different endpoint ports remain distinct in connection pools and multiplexing.

Encode spaces as `%20`, `%` as `%25`, `#` as `%23`, and `?` as `%3F`.
Percent escapes must decode to valid UTF-8; NUL, malformed authorities, invalid
ports, and unescaped query or fragment delimiters are rejected before remote
source expansion. URI spelling does not change the transport backend or remote
path safety rules. Source `*` expansion still applies after decoding.

## Background jobs and completion

Background workers replay the connection options from the original command:

```bash
bcmr --ssh-port 2222 copy --background --json report.pdf storage:backup/
```

Generated Bash, Zsh, Fish, and PowerShell remote completion forwards the
connection options already typed, including repeated identities, to the remote
completion request. Re-evaluate `bcmr init` or regenerate installed completion
scripts after upgrading. See [shell integration](./shell-integration.md).
