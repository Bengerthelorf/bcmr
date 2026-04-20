---
title: Progress Display
section: guide
order: 4
---

BCMR provides two progress display modes for all file operations.

## Fancy Mode (Default)

A TUI box with:

- Total progress bar with color gradient
- Transfer speed and ETA
- Current file name and per-file progress bar
- Item count (for remove operations)
- Scanning indicator (pipeline mode shows files found in real time)

Supports Ctrl+C (clean exit with partial file cleanup) and Ctrl+Z (suspend/resume on Unix).

## Plain Mode

A 3-line text display suitable for logs, pipes, and terminals without box-drawing support.

Enable with `--tui` / `-t` flag, or set `progress.style = "plain"` in config.

```
Copying: [=========-----------] 45%
12.34 MiB / 27.00 MiB | 5.67 MiB/s | ETA: 00:03
File: largefile.zip [====----] 50%
```

## Pipeline Scanning

When no overwrite prompt or dry-run is needed, BCMR uses pipeline mode — copying starts immediately while directories are still being scanned. The progress display shows a scanning animation with the file count updating in real time, then switches to the normal progress view once scanning completes.

## Customization

See [Configuration](/guide/configuration) for color gradients, bar characters, and border styles.
