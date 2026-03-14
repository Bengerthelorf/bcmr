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

## How It Works

- Uses your existing SSH configuration (`~/.ssh/config`, keys, etc.)
- Validates SSH connectivity before starting transfers
- Streams data through SSH with progress tracking
- Supports both upload and download directions

::: warning Limitations
- Cannot copy between two remote hosts directly — use a local intermediary
- SSH must be configured for non-interactive (key-based) authentication (`BatchMode=yes`)
- Excludes and advanced copy features (reflink, sparse, resume) are not available for remote transfers
:::

## Path Detection

BCMR detects remote paths by the `[user@]host:path` format. These patterns are recognized as local paths and will not trigger remote mode:

- Absolute paths (`/path/to/file`)
- Relative paths (`./file`, `../file`)
- Home directory (`~/file`)
- Windows drive letters (`C:\file`)
