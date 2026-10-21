---
cmd: bcmr copy
group: core
sig: <sources>... <destination>
desc: copy local or ssh files and directories, with optional blake3 readback verification
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
  - { f: "--to <DEST>",         t: "path...",                d: "—",          x: "fan-out target (repeatable); all positionals become sources" }
  - { f: "--no-deref",          t: bool,                     d: "false",      x: "local copies only: replicate symlinks instead of following them (cp -P)" }
  - { f: "-V, --verify",        t: bool,                     d: "false",      x: "recompute blake3 after copy" }
  - { f: "-C, --resume",        t: bool,                     d: "false",      x: "resume interrupted copies using available saved state and hashes" }
  - { f: "-s, --strict",        t: bool,                     d: "false",      x: "strict blake3 partial-hash resume" }
  - { f: "-a, --append",        t: bool,                     d: "false",      x: "append to a partial file; local checks size, remote checks prefix hash" }
  - { f: "--sync",              t: bool,                     d: "false",      x: "fsync after copy" }
  - { f: "--reflink",           t: "auto|force|disable",     d: "auto",       x: "copy-on-write mode" }
  - { f: "--sparse",            t: "auto|force|disable",     d: "auto",       x: "sparse file handling" }
  - { f: "-P, --parallel <N>",  t: int,                      d: "config",     x: "parallel ssh transfers for remote copy" }
  - { f: "-j, --jobs <N>",      t: int,                      d: "min(CPU,8)", x: "max concurrent local file copies" }
  - { f: "--compress",          t: "auto|zstd|lz4|none",     d: "auto",       x: "remote-wire compression" }
  - { f: "--fast",              t: bool,                     d: "false",      x: "remote only: skip server-side blake3; linux splice(2)" }
  - { f: "--direct",            t: "ssh|direct",             d: "ssh",        x: "remote data plane: ssh stream or direct TCP with AES-256-GCM" }
  - { f: "--progress",          t: "auto|tui|inline|plain|off", d: "auto",      x: "choose the progress display" }
  - { f: "-q, --quiet",         t: bool,                     d: "false",      x: "suppress progress UI; errors only" }
  - { f: "--json",              t: bool,                     d: "false",      x: "emit NDJSON events and a terminal result; remain in foreground" }
  - { f: "-b, --background",    t: bool,                     d: "false",      x: "submit a background job; query with bcmr status" }
  - { f: "--config <PATH>",     t: path,                     d: "—",          x: "layer this file over discovered configuration" }
  - { f: "--profile <NAME>",    t: str,                      d: "—",          x: "apply named argument defaults; overrides BCMR_PROFILE" }
example:
  - "scanning source ……  2,481 files · 14.2 gb"
  - "handshake (ssh) ……  ok"
  - "direct-tcp fast path ……  negotiated"
  - "transfer ……  412 mb/s · eta 00:00:12"
  - "complete · all blake3 verified"
---

`bcmr copy` operates over local paths, remote ssh paths, or a mix. Use
`-r` for directories and `-V` to request BLAKE3 readback verification.
Local reflink and other copy strategies can avoid streaming every byte
through the application; readback verification is disabled by default.

`--json` selects structured output. Add `--background` to submit a job
instead of waiting for completion.

Local resume modes (`-C`, `-s`, `-a`) differ in how they validate existing
data:

- **`-C`** — resume from verified chunks in a matching saved session. Without
  usable session proof, a shorter file is recopied; equal-size files are
  skipped only after a full hash match.
- **`-s`** — compare the existing file's BLAKE3 hash with the source prefix;
  append on a match, otherwise recopy.
- **`-a`** — trust existing data: append if smaller, skip if equal size, reject
  a larger destination. This local mode does not validate the existing prefix.

Remote resume and append validate the existing prefix by hash. Remote
`--append` rejects a mismatched or oversized destination.


## Source stars

Source paths support `*` locally and remotely. For example:

```bash
bcmr copy 'host:reports/report*' ./downloads/
```

Quote patterns for direct bcmr calls. Initialized zsh copy/move wrappers accept
unquoted stars through command-specific `noglob` aliases. Stars do not cross
`/`, hidden names require a leading dot, and directories still require `-r`.
No matches is an error before writes; exact existing paths and destinations
remain literal. Only `*` is expanded. Fan-out resolves matches once, and move
removes only that resolved source set after every target succeeds.


## Destination paths

Existing directories and paths ending in `/` are parent directories, including
missing ones: `bcmr copy report.pdf new/parent/` creates `new/parent/report.pdf`.
A single bare missing destination names the new file or directory. Multiple
sources require an existing directory or an explicit trailing `/`.
Dry-run uses the same mapping without creating directories.
