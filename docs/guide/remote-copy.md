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

## How It Works

- Uses your existing SSH configuration (`~/.ssh/config`, keys, etc.)
- Validates SSH connectivity before starting transfers
- Reuses SSH connections via ControlMaster multiplexing
- Streams data through SSH with progress tracking
- Supports both upload and download directions

::: warning Limitations
- Cannot copy between two remote hosts directly — use a local intermediary
- Excludes and advanced copy features (reflink, sparse, resume) are not available for remote transfers
:::

## Path Detection

BCMR detects remote paths by the `[user@]host:path` format. These patterns are recognized as local paths and will not trigger remote mode:

- Absolute paths (`/path/to/file`)
- Relative paths (`./file`, `../file`)
- Home directory (`~/file`)
- Windows drive letters (`C:\file`)
