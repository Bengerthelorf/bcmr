---
cmd: bcmr ls
group: remote
sig: <host:path>
desc: list files under a remote path without opening a shell
tags: [stable]
order: 1
related: [bcmr stat, bcmr du, bcmr hash]
flags:
  - { f: "<path>", t: "host:path | @bookmark", d: "—", x: "remote path to list" }
  - { f: "--json", t: bool, d: "false", x: "emit a structured directory listing" }
  - { f: "--config <PATH>", t: path, d: "—", x: "layer this file over discovered configuration" }
example:
  - "d        -  src"
  - "- 14.2 MB  data.bin"
  - "-  1.1 KB  notes.md"
---

Lists a remote directory over the same connection machinery as
`bcmr copy` — SSH multiplexing and the serve fast path when available.
Accepts `@bookmark` aliases from the `[paths]` config table. `--json`
emits `{path, entries: [{type, size, name}]}` for scripts and agents.
