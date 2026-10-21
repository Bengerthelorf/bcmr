---
cmd: bcmr check
group: core
sig: <sources>... <destination>
desc: compare source and destination using size, mtime, and blake3
tags: [stable]
order: 4
related: [bcmr copy]
flags:
  - { f: "-r, --recursive",     t: bool,       d: "false", x: "recursively compare directories" }
  - { f: "-e, --exclude",       t: "regex...", d: "—",     x: "exclude paths matching regex" }
  - { f: "--no-hash",           t: bool,       d: "false", x: "skip content hashing; flag size-matched, mtime-drifted files as modified" }
  - { f: "--json",              t: bool,       d: "false", x: "emit a structured comparison report" }
  - { f: "--config <PATH>",     t: path,       d: "—",     x: "layer this file over discovered configuration" }
  - { f: "--profile <NAME>",    t: str,        d: "—",     x: "apply named argument defaults; overrides BCMR_PROFILE" }
example:
  - "comparing ./src  ./backup"
  - "added:    1 file"
  - "modified: 2 files"
  - "missing:  0 files"
---

Use `-r` for directory comparisons, including with `--json` or `--no-hash`.

An existing destination directory is treated as a parent. For example,
`bcmr check -r ./src/ ./backup/` compares `./src/` with `./backup/src/`.
Create the destination parent before copying so copy and check use the
same layout:

```sh
mkdir -p ./backup
bcmr copy -r ./src/ ./backup/
bcmr check -r ./src/ ./backup/
```

Copying to a fresh path, such as `bcmr copy -r ./src/ ./new-name/`, instead
creates a renamed root at `./new-name/`. Passing that now-existing directory
to check would look for `./new-name/src/`.

Directory comparisons trust files with matching size and mtime. By default,
size-matched files whose mtimes differ are hashed to confirm whether their
content changed. Explicit file-to-file comparisons always hash the files
unless `--no-hash` is given. A recursive check does not rehash every file.

Exit codes: `0` = in sync, `1` = differences found or a human-output runtime
error, `2` = a JSON runtime error, invalid CLI arguments, or a configuration
error, and `64` = a missing or unparseable explicit config or unknown
profile.

`bcmr check --json` emits a structured report and a `summary` object.
Nonempty `added`, `modified`, and `missing` arrays describe differences;
empty arrays are omitted.
