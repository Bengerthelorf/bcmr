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
example:
  - "scanning ……  37 files · 120.4 mb"
  - "confirm? y"
  - "removed · all good"
---
