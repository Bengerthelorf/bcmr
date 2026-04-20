---
title: Getting Started
section: guide
order: 1
---

BCMR (Better Copy Move Remove) is a modern CLI tool for file operations,
written in Rust. It offers progress tracking, resume support, integrity
verification, and SSH remote copy.

If you don't have it yet, head to [**install**](/install) first — this page
assumes the `bcmr` binary is on your `$PATH`.

## Quick Start

```bash
# Copy a file
bcmr copy document.txt backup/

# Recursively copy a directory
bcmr copy -r projects/ backup/

# Move files
bcmr move old_file.txt new_location/

# Remove with confirmation
bcmr remove -r old_project/

# Dry run — preview without changes
bcmr copy -r -n projects/ backup/
```

:::callout[Shell Integration]{kind="info"}
Set up shell aliases so `cp`, `mv`, `rm` (or your own prefix) automatically
route through bcmr. See [Shell Integration](/guide/shell-integration).
:::

## Next Steps

- [Shell Integration](/guide/shell-integration) — replace or alias native commands
- [Configuration](/guide/configuration) — colors, progress style, copy behavior
- [Remote Copy](/guide/remote-copy) — SSH + direct-tcp fast-path
- [CLI Reference](/commands) — every subcommand, every flag, searchable
