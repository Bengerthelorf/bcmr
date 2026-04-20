---
cmd: bcmr move
group: core
sig: <sources>... <destination>
desc: move files; same-device is instant rename, cross-device falls back to copy+delete with verify
tags: [stable, core]
order: 2
related: [bcmr copy, bcmr remove]
flags:
  - { f: "-r, --recursive",     t: bool,                 d: "false",      x: "recursively move directories" }
  - { f: "-p, --preserve",      t: bool,                 d: "false",      x: "preserve attributes" }
  - { f: "-f, --force",         t: bool,                 d: "false",      x: "overwrite existing files" }
  - { f: "-y, --yes",           t: bool,                 d: "false",      x: "skip overwrite confirmation prompt" }
  - { f: "-v, --verbose",       t: bool,                 d: "false",      x: "explain what is being done" }
  - { f: "-e, --exclude",       t: "regex...",           d: "—",          x: "exclude paths matching regex" }
  - { f: "-n, --dry-run",       t: bool,                 d: "false",      x: "preview without making changes" }
  - { f: "-V, --verify",        t: bool,                 d: "false",      x: "verify file integrity after move" }
  - { f: "-C, --resume",        t: bool,                 d: "false",      x: "resume interrupted move (cross-device only)" }
  - { f: "-s, --strict",        t: bool,                 d: "false",      x: "strict hash-verified resume" }
  - { f: "-a, --append",        t: bool,                 d: "false",      x: "append mode for cross-device" }
  - { f: "--sync",              t: bool,                 d: "false",      x: "fsync after (cross-device only)" }
  - { f: "-j, --jobs <N>",      t: int,                  d: "min(CPU,8)", x: "max concurrent copies on fallback" }
  - { f: "--compress",          t: "auto|zstd|lz4|none", d: "auto",       x: "wire compression for remote moves" }
  - { f: "--fast",              t: bool,                 d: "false",      x: "remote only: skip server-side blake3 on copy phase" }
example:
  - "move ./notes ~/archive ……  rename(2)"
  - "complete · 1 path moved"
---

Same-device moves use `rename(2)` and complete instantly. Cross-device
moves fall back to copy + delete with progress tracking and all the
integrity guarantees of `bcmr copy`.
