---
cmd: bcmr remove
group: core
sig: <paths>...
desc: remove files and directories, with dry-run, interactive, and regex-exclude modes
tags: [stable, core]
order: 3
related: [bcmr copy, bcmr move]
flags:
  - { f: "-r, --recursive",     t: bool,           d: "false", x: "recursively remove directories" }
  - { f: "-f, --force",         t: bool,           d: "false", x: "force removal without confirmation" }
  - { f: "-y, --yes",           t: bool,           d: "false", x: "skip confirmation prompt" }
  - { f: "-i, --interactive",   t: bool,           d: "false", x: "prompt before each removal" }
  - { f: "-v, --verbose",       t: bool,           d: "false", x: "explain what is being done" }
  - { f: "-d, --dir",           t: bool,           d: "false", x: "remove empty directories only (rmdir)" }
  - { f: "-e, --exclude",       t: "regex...",     d: "—",     x: "exclude paths matching regex" }
  - { f: "-n, --dry-run",       t: bool,           d: "false", x: "preview without making changes" }
  - { f: "--progress",          t: "auto|tui|inline|plain|off", d: "auto", x: "choose the progress display" }
  - { f: "-q, --quiet",         t: bool,           d: "false", x: "suppress progress and per-file messages" }
  - { f: "--json",              t: bool,           d: "false", x: "emit NDJSON events and a terminal result; remain in foreground" }
  - { f: "-b, --background",    t: bool,           d: "false", x: "submit a background job; query with bcmr status" }
  - { f: "--config <PATH>",     t: path,           d: "—",     x: "layer this file over discovered configuration" }
  - { f: "--profile <NAME>",    t: str,            d: "—",     x: "apply named argument defaults; overrides BCMR_PROFILE" }
example:
  - "scanning ……  37 files · 120.4 mb"
  - "confirm? y"
  - "removed · all good"
---

JSON and background removal require explicit consent with `--yes` or
`--force`, unless using `--dry-run`. These consent flags override `--interactive`;
an effective per-item prompt requires a terminal and cannot run in JSON or background mode.

Local exclusions prune whole directory subtrees. Directory symlinks, including
operands ending in `/`, are removed as links. `--dir` removes only an empty
directory and fails if a file appears while confirmation is pending.

Remote removal currently supports one aggregate confirmation, not `--interactive`
per-item prompts or `--exclude`. Unsupported options are rejected before any
selected path is removed. `--dry-run` previews without asking for confirmation.
