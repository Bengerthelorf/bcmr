---
title: Progress Display
section: guide
order: 4
---

BCMR provides five explicit progress modes for all file operations. Select one with
`--progress=auto|tui|inline|plain|off`, or set `progress.style` to the same value.

## Auto Mode (Default)

Auto keeps live progress below the command, preserving the terminal screen and shell history:

- Inline progress on a TTY with enough space, including when `NO_COLOR` is set
- Plain final summaries for pipes, redirected output, and `TERM=dumb`

Verbose (`-v`) operations use stable line output so per-file messages remain in
scrollback. Interactive removal shows prompts without a live progress display.

## TUI Mode

Select `--progress=tui` explicitly for a full-screen TUI box with:

- Total progress bar with color gradient
- Transfer speed and ETA
- Current file name and per-file progress bar
- Item count (for remove operations)
- Scanning indicator (pipeline mode shows files found in real time)

Supports Ctrl+C (clean exit with partial file cleanup) and Ctrl+Z (suspend/resume on Unix).

## Inline Mode

An uncolored 3-line live display for terminals where a full TUI is undesirable:

```
Copying: [=========-----------] 45%
12.34 MiB / 27.00 MiB | 5.67 MiB/s | ETA: 00:03
File: largefile.zip [====----] 50%
```

## Plain and Off Modes

`plain` prints a stable final summary and never uses cursor controls, so it is safe
for logs and pipelines. `off` suppresses progress entirely; `-q` / `--quiet` is the
convenient shortcut and also takes precedence over `-v`. Dry-run previews and
required confirmation prompts are still shown.

## Pipeline Scanning

When no overwrite prompt or dry-run is needed, BCMR uses pipeline mode — copying starts immediately while directories are still being scanned. The progress display shows a scanning animation with the file count updating in real time, then switches to the normal progress view once scanning completes.

## Customization

See [Configuration](/guide/configuration) for color gradients, bar characters, and border styles.
