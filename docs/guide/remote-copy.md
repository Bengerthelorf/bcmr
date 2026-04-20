# Remote Copy (SSH)

BCMR supports copying files to and from remote hosts using SCP-like syntax over SSH.

## Syntax

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

Default parallel count is configured in `[scp] parallel_transfers` (default: 4). When `-P 1` or omitted on small transfers, files are sent sequentially.

Both TUI and plain text modes show per-worker status:

```
Uploading: [████████░░░░░░░░░░░░░░░░░░] 42% [3/4w]
150 MiB / 350 MiB | 45.5 MiB/s | ETA: 04:32
[1] large.iso 53% | [2] backup.tar 78% | [3] data.csv 12% | [4] idle
```

## Compression

BCMR has **two separate** compression layers. Know the difference:

### SSH-Level (legacy SCP path)

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

When the remote host also has bcmr installed, transfers automatically use the **bcmr serve protocol** — a binary frame protocol over a single SSH connection. This eliminates per-file SSH process overhead and enables server-side hashing.

If the remote doesn't have bcmr, it falls back to legacy SCP transparently.

### Installing bcmr on Remote

```bash
# Deploy bcmr to a remote host
bcmr deploy user@host

# Custom install path
bcmr deploy user@host --path /usr/local/bin/bcmr
```

`bcmr deploy` detects the remote OS and architecture. If the remote matches your local platform, it transfers your local binary directly. Otherwise, it downloads the correct binary from GitHub Releases.

### Serve Protocol Benefits

| | Legacy SSH | Serve Protocol |
|---|---|---|
| Connection setup | New process per file | Single persistent connection |
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

### Content-Addressed Dedup (`CAP_DEDUP`)

The serve protocol includes block-level dedup: uploads ≥ 16 MiB
first exchange BLAKE3 hashes of each 4 MiB block, and the server
only asks for the blocks it doesn't already have in its local
content-addressed store (CAS). The CAS path is governed by
[`BCMR_CAS_DIR` / `BCMR_CAS_CAP_MB`](/guide/configuration#environment-variables).

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
- **Legacy mode**: reuses SSH connections via ControlMaster multiplexing, parallel workers use independent TCP connections
- Streams data through SSH with progress tracking
- Supports both upload and download directions

::: warning Limitations
- Cannot copy between two remote hosts directly — use a local intermediary
- Resume (`-C`) on serve fast path: single-file uploads are supported natively. Recursive directory uploads and downloads with `--resume/--strict/--append` fall back to legacy mode automatically.
:::

## Path Detection

BCMR detects remote paths by the `[user@]host:path` format. These patterns are recognized as local paths and will not trigger remote mode:

- Absolute paths (`/path/to/file`)
- Relative paths (`./file`, `../file`)
- Home directory (`~/file`)
- Windows drive letters (`C:\file`)
