---
cmd: bcmr copy
group: core
sig: <sources>... <destination>
desc: copy files or directories with blake3 verification; src and dst may be local or ssh
tags: [stable, core]
order: 1
related: [bcmr move, bcmr check, bcmr remove]
flags:
  - { f: "-r, --recursive",     t: bool,                     d: "false",      x: "recursively copy directories" }
  - { f: "-p, --preserve",      t: bool,                     d: "false",      x: "preserve permissions and timestamps" }
  - { f: "-f, --force",         t: bool,                     d: "false",      x: "overwrite existing files" }
  - { f: "-y, --yes",           t: bool,                     d: "false",      x: "skip overwrite confirmation prompt" }
  - { f: "-v, --verbose",       t: bool,                     d: "false",      x: "explain what is being done" }
  - { f: "-e, --exclude",       t: "regex...",               d: "—",          x: "exclude paths matching regex (repeatable)" }
  - { f: "-n, --dry-run",       t: bool,                     d: "false",      x: "preview without making changes" }
  - { f: "-V, --verify",        t: bool,                     d: "false",      x: "recompute blake3 after copy" }
  - { f: "-C, --resume",        t: bool,                     d: "false",      x: "resume interrupted copy (size + mtime check)" }
  - { f: "-s, --strict",        t: bool,                     d: "false",      x: "strict blake3 partial-hash resume" }
  - { f: "-a, --append",        t: bool,                     d: "false",      x: "append if dst smaller (size check only)" }
  - { f: "--sync",              t: bool,                     d: "false",      x: "fsync after copy" }
  - { f: "--reflink",           t: "auto|force|disable",     d: "auto",       x: "copy-on-write mode" }
  - { f: "--sparse",            t: "auto|force|disable",     d: "auto",       x: "sparse file handling" }
  - { f: "-P, --parallel <N>",  t: int,                      d: "config",     x: "parallel ssh transfers for remote copy" }
  - { f: "-j, --jobs <N>",      t: int,                      d: "min(CPU,8)", x: "max concurrent local file copies" }
  - { f: "--compress",          t: "auto|zstd|lz4|none",     d: "auto",       x: "remote-wire compression" }
  - { f: "--fast",              t: bool,                     d: "false",      x: "remote only: skip server-side blake3; linux splice(2)" }
example:
  - "scanning source ……  2,481 files · 14.2 gb"
  - "handshake (ssh) ……  ok"
  - "direct-tcp fast path ……  negotiated"
  - "transfer ……  412 mb/s · eta 00:00:12"
  - "complete · all blake3 verified"
---

`bcmr copy` is the primary verb. It operates over local paths, remote ssh
paths, or a mix. Every byte streams through a blake3 hasher during write —
the destination is only sealed once the hash of the bytes on disk matches
the source.

Resume modes (`-C`, `-s`, `-a`) differ by what they compare before
deciding to append vs. re-copy:

- **`-C`** — mtime match → append; mismatch → overwrite
- **`-s`** — blake3 partial hash match → append; mismatch → overwrite
- **`-a`** — always append if dst is smaller; skip if same size; overwrite if larger
