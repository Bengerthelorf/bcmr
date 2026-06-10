---
cmd: bcmr check
group: core
sig: <sources>... <destination>
desc: compare source and destination; size + mtime triage, blake3 confirms size-matched files
tags: [stable]
order: 4
related: [bcmr copy]
flags:
  - { f: "-r, --recursive",     t: bool,       d: "false", x: "recursively compare directories" }
  - { f: "-e, --exclude",       t: "regex...", d: "—",     x: "exclude paths matching regex" }
  - { f: "--no-hash",           t: bool,       d: "false", x: "skip content hashing; flag size-matched, mtime-drifted files as modified" }
example:
  - "comparing ./src  ./backup"
  - "added:    1 file"
  - "modified: 2 files"
  - "missing:  0 files"
---

Exit codes: `0` = in sync, `1` = differences found, `2` = error.

`bcmr check --json` emits a structured report with `added`, `modified`,
and `missing` arrays, useful for CI pipelines and AI agents.
