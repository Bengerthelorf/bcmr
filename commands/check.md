---
cmd: bcmr check
group: core
sig: <sources>... <destination>
desc: compare source and destination (size + mtime, no hashing); reports added, modified, missing
tags: [stable]
order: 4
related: [bcmr copy]
flags:
  - { f: "-r, --recursive",     t: bool,       d: "false", x: "recursively compare directories" }
  - { f: "-e, --exclude",       t: "regex...", d: "—",     x: "exclude paths matching regex" }
example:
  - "comparing ./src  ./backup"
  - "added:    1 file"
  - "modified: 2 files"
  - "missing:  0 files"
---

Exit codes: `0` = in sync, `1` = differences found, `2` = error.

`bcmr check --json` emits a structured report with `added`, `modified`,
and `missing` arrays, useful for CI pipelines and AI agents.
