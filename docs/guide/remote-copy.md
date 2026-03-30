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

SSH compression can be enabled to reduce transfer time over slow links. Configure in `[scp]`:

| Value | Behavior |
|-------|----------|
| `"auto"` | Enable compression if >30% of transfer bytes are compressible by extension (default) |
| `"force"` | Always enable SSH compression |
| `"off"` | Never compress |

In `auto` mode, files with known compressed extensions (`.gz`, `.zip`, `.mp4`, `.jpg`, etc.) are counted as incompressible. If the majority of bytes are already compressed, compression is skipped to avoid CPU overhead.

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

## How It Works

- Uses your existing SSH configuration (`~/.ssh/config`, keys, etc.)
- Validates SSH connectivity before starting transfers
- **Serve mode**: launches `bcmr serve` on remote via SSH, communicates via binary protocol over stdin/stdout
- **Legacy mode**: reuses SSH connections via ControlMaster multiplexing, parallel workers use independent TCP connections
- Streams data through SSH with progress tracking
- Supports both upload and download directions

::: warning Limitations
- Cannot copy between two remote hosts directly — use a local intermediary
- Resume (`-C`) is not yet available for serve-based remote transfers (use legacy mode with `-P 1`)
:::

## Path Detection

BCMR detects remote paths by the `[user@]host:path` format. These patterns are recognized as local paths and will not trigger remote mode:

- Absolute paths (`/path/to/file`)
- Relative paths (`./file`, `../file`)
- Home directory (`~/file`)
- Windows drive letters (`C:\file`)
