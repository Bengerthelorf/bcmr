---
title: Remote Copy (SSH)
section: guide
order: 5
---

BCMR supports copying files to and from remote hosts using SCP-like syntax over SSH.

## Syntax

### Source stars

Copy and move expand `*` in local or remote source paths. Quote patterns when
calling bcmr directly so your shell passes them through:

```bash
bcmr copy 'mini_m2:/Users/snaix/Downloads/deecamp中期检查*' ~/Downloads/
bcmr copy 'host:~/reports/report*' ./downloads/
```

After evaluating `bcmr init zsh --cmd b`, `bcp` and `bmv` also accept unquoted
stars. Their command-specific `noglob` aliases leave zsh's `NOMATCH` setting
and other commands unchanged. Reload initialization after upgrading.

`*` matches zero or more characters in one path component; it does not cross
`/`. Hidden names require a leading dot, and matching directories still
require `-r`. No matches is an error before copying. An exact existing path
is kept literal, including a filename containing `*`; destinations are always
literal. Only `*` has pattern meaning (`?`, brackets and recursive `**` are
not additional pattern operators). Fan-out resolves one shared source snapshot.

For remaining differences from scp, see [scp compatibility](./scp-compatibility.md).

```bash
# Upload: local → remote
bcmr copy local_file.txt user@host:/remote/path/

# Download: remote → local
bcmr copy user@host:/remote/file.txt ./local/

# Recursive upload
bcmr copy -r local_dir/ user@host:/remote/path/

# Recursive download
bcmr copy -r user@host:/remote/dir/ ./local/
```

## Parallel Transfers

Transfer multiple files concurrently with the `-P` flag:

```bash
# Upload 4 files in parallel
bcmr copy -P 4 file1.bin file2.bin file3.bin file4.bin user@host:/remote/

# Recursive upload with 8 workers
bcmr copy -r -P 8 ./large_dataset/ user@host:/data/

# Download multiple files in parallel
bcmr copy -P 3 user@host:/data/a.bin user@host:/data/b.bin ./local/
```

Default parallel count is configured in `[scp] parallel_transfers`
(default: 4). Serve-mode directory batches use at most that many
independent SSH sessions. A single upload opens one session even
when `-P` is larger, because the production upload path deliberately
does not stripe one visible file across connections.

Both TUI and plain text modes show per-worker status:

```
Uploading: [████████░░░░░░░░░░░░░░░░░░] 42% [3/4w]
150 MiB / 350 MiB | 45.5 MiB/s | ETA: 04:32
[1] large.iso 53% | [2] backup.tar 78% | [3] data.csv 12% | [4] idle
```

## Compression

BCMR has **two separate** compression layers. Know the difference:

### SSH-Level (legacy SSH path)

When the remote doesn't have bcmr installed and transfers fall back
to SCP, the SSH transport can compress with zlib. Configure in
`[scp]` of the config file:

| Value | Behavior |
|-------|----------|
| `"auto"` | Enable compression if >30% of transfer bytes are compressible by extension (default) |
| `"force"` | Always enable SSH compression |
| `"off"` | Never compress |

In `auto` mode, files with known compressed extensions (`.gz`, `.zip`, `.mp4`, `.jpg`, etc.) are counted as incompressible. If the majority of bytes are already compressed, compression is skipped to avoid CPU overhead.

### Wire-Level (serve protocol, `--compress`)

When both sides speak the [serve protocol](#serve-protocol-accelerated-transfers),
per-block LZ4 or Zstd compression is negotiated in the handshake.
This is **separate from and faster than** SSH's zlib — Zstd-3
runs at ~320 MB/s encode with ~5× reduction on source text, vs
zlib at roughly a tenth of that.

| `--compress` mode | Caps advertised | Negotiated result |
|---|---|---|
| `auto` (default) | LZ4 + Zstd | Zstd-3 if both peers agree, else LZ4, else raw |
| `zstd` | Zstd only | Zstd-3 if server also has it, else raw |
| `lz4` | LZ4 only | LZ4 if server also has it, else raw |
| `none`/`off` | none | raw `Data` frames only |

Each 4 MiB block is auto-skipped (sent raw) when compression would
bring it to more than 95 % of the original — so already-compressed
files (`.jpg`, `.zst`, `/dev/urandom`) pay almost nothing for
having compression enabled. See the
[Wire Protocol ablation](/ablation/wire-protocol#experiment-12-wire-compression-across-real-hosts)
for measured ratios and throughput across real links.

## Serve Protocol (Accelerated Transfers)

When the remote host also has a matching bcmr version, transfers
automatically use the **bcmr serve protocol** — a binary frame
protocol over persistent SSH sessions. This eliminates per-file SSH
process overhead and enables server-side hashing.

The current wire version is 2 and intentionally does not accept v1
PUT semantics. If the remote is missing bcmr or has a mismatched
protocol, bcmr prints a fallback warning and uses legacy SSH shell commands.

### Installing bcmr on Remote

```bash
# Deploy bcmr to a remote host
bcmr deploy user@host

# Upgrade a system installation (sudo can prompt in an interactive terminal)
bcmr deploy user@host --path /usr/local/bin/bcmr --sudo

# Independent deployment to multiple hosts with shared options
bcmr deploy A100 4090 --path /usr/bin/bcmr --sudo
```

`bcmr deploy` detects each host's OS and architecture. Matching platforms receive the local executable; other platforms download the matching asset from the release with the local version number.

Version checks and post-install verification use the selected installation path (default `~/.local/bin/bcmr`), independently of another installation on PATH. Installed and skipped results show both the selected path/version and the PATH-resolved path/version, with a warning if PATH selects another executable. Use `--path` to upgrade that specific installation.

Hosts run sequentially. A failed host does not stop later hosts; the final summary lists installed, skipped and failed results. Any failure gives a nonzero exit status. Rerunning skips selected installations already at the local version.

`--sudo` uses a native SSH terminal for password authentication when stdin and stderr are terminals. Non-interactive and `--json` runs use `sudo -n` and report missing passwordless permissions. SSH login still uses configured keys in batch mode.

### Serve Protocol Benefits

| | Legacy SSH | Serve Protocol |
|---|---|---|
| Connection setup | New process per file | Persistent session or bounded pool |
| File listing | `ssh find` (shell parsing) | Binary LIST message |
| Hash verification | Transfer data back to local | Server-side BLAKE3 computation |
| Upload verification | Re-download to verify | Server returns hash in PUT response |
| Per-file overhead | ~50ms (process spawn) | ~0.1ms (message frame) |

### Verifying Remote Transfers

```bash
# Upload with integrity verification
bcmr copy -V local_file.txt user@host:/backup/

# With serve protocol, the server computes the hash after writing
# and returns it — no need to re-transfer data for verification
```

### Atomic Upload and Overwrite Policy

Serve PUT is no-clobber by default. The overwrite decision is
carried on protocol v2 and enforced again by the server:

```bash
# Fails if report.pdf already exists remotely
bcmr copy report.pdf user@host:/backup/

# Durable atomic replacement (requires negotiated serve CAP_SYNC)
bcmr copy --force --yes --sync report.pdf user@host:/backup/

# Explicit verification before publication
bcmr copy --force --yes --verify report.pdf user@host:/backup/
```

The server receives into a private handle-bound transaction,
validates the declared length and hash, and only then publishes the
file. An interrupted forced replacement retains the old final path.
Pipelined directory uploads keep this property per file.

Fresh legacy SSH uploads also use a private stage. Uploads with `-p` or `-V`
finalize attributes and verification in a private stage before publication.
Those SSH-assisted final renames cannot provide checked directory fsync:
`--sync` therefore refuses legacy uploads, link-preserving uploads, and uploads
combining `--sync` with `-p` or `-V`, before any destination writes. Ordinary
serve uploads require every connection to negotiate `CAP_SYNC`; a missing
capability fails explicitly. Synced downloads sync local publication on Unix.

Copy follows links by default; `--no-deref` preserves link objects, including
dangling links. Move always preserves link objects. The link-preserving remote
route uses SSH helpers and refuses `--direct direct`; dry-run can still preview
these operations. See [local and remote behavior](./local-remote-semantics.md)
for exclusions, directory attributes, move cleanup and platform boundaries.

Single-file striped PUT is currently disabled in production because
independent connections cannot yet join the same server-owned
transaction. `-P N` still parallelizes files in recursive batches;
it does not trade away crash safety to stripe one upload.

### Content-Addressed Dedup (`CAP_DEDUP`)

The serve protocol includes block-level dedup: uploads ≥ 16 MiB
first exchange BLAKE3 hashes of each 4 MiB block, and the server
only asks for the blocks it doesn't already have in its local
content-addressed store (CAS). The CAS path is governed by
[`BCMR_CAS_DIR` / `BCMR_CAS_CAP_MB`](/guide/configuration#environment-variables).

In 0.7.1, Linux/macOS serve caches are private and scoped to the captured root directory
identity. Different roots cannot reuse each other's blocks, and all served-root
namespaces share one capacity budget, separate from the local CAS. The first
upload after upgrading warms this scoped cache. Other platforms, including FreeBSD and Windows, request all
blocks without caching them. Unavailable or unsafe cache storage disables this
optional acceleration while ordinary transfers continue.

This fires automatically on every PUT of a large enough file —
no flag is needed to enable it. The benefit is obvious only on
repeat uploads of the same artifact (dev-loop style): the second
upload skips every block the server already has.

```bash
# First run: all 64 MiB on the wire
bcmr copy build/artifact.bin user@host:/deploy/

# Second run: every block is a CAS hit, no bytes on the wire
bcmr copy build/artifact.bin user@host:/deploy/alt-name.bin
```

See the [dedup experiment](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put)
for the protocol trace.

### Fast Mode (`--fast`)

Trades server-side BLAKE3 for lower CPU:

```bash
# Server's Ok response carries hash:None.
bcmr copy --fast user@host:/big.bin ./local.bin

# Combine with -V to re-hash the dst client-side:
bcmr copy --fast -V user@host:/big.bin ./local.bin
```

When the server is Linux and `--compress=none` is also set,
`--fast` additionally engages `splice(2)` for the file → stdout
path. The splice implementation is currently
[not yet a win in all cases](/ablation/wire-protocol#experiment-14-cap-fast-real-numbers);
`--fast` is honest about this trade-off and documented in the
Internals.

Default is always off — `--fast` is an explicit opt-out of
server-side integrity verification.

## How It Works

- Uses your existing SSH configuration (`~/.ssh/config`, keys, etc.)
- Validates SSH connectivity before starting transfers
- **Serve mode**: launches `bcmr serve` on remote via SSH, communicates via binary protocol over stdin/stdout
- **Legacy mode**: reuses SSH connections via ControlMaster by
  default; `BCMR_SSH_NO_MULTIPLEX=1` opts out when the remote sshd is
  tuned for independent connection bursts
- **Serve pool**: explicitly disables ControlMaster so parallel
  members are independent TCP/cipher streams even when user SSH
  config enables multiplexing
- Probes silent SSH sessions every 15 seconds and closes them after
  20 unanswered probes; slow sessions with continuing traffic remain
  valid
- Synced downloads resolve the deepest existing directory prefix
  once before durable creation. Existing symlinked home or storage
  prefixes work; a broken symlink still fails without creating data
  at an ambiguous destination
- Streams data through SSH with progress tracking
- Supports both upload and download directions

:::callout[Limitations]{kind="warn"}
- Remote-to-remote transfers stream through this client; see [transfer backends](./transfer-backends.md).
- Resume (`-C`) on serve fast path: single-file uploads are supported natively. Recursive directory uploads and downloads with `--resume/--strict/--append` fall back to legacy mode automatically.
- A killed upload in a shared writable destination directory may
  retain an empty private `.bcmr.receive.*` recovery directory.
  BCMR refuses unsafe name-based cleanup in a raceable namespace.
:::

## Path Detection

BCMR detects remote paths by the `[user@]host:path` format. These patterns are recognized as local paths and will not trigger remote mode:

- Absolute paths (`/path/to/file`)
- Relative paths (`./file`, `../file`)
- Home directory (`~/file`)
- Windows drive letters (`C:\file`)

See [local/remote behavior](./local-remote-semantics.md) for destination rules and remaining differences.

See [transfer backends](./transfer-backends.md) for mixed sources, aggregate bandwidth limits and SFTP-only accounts.
