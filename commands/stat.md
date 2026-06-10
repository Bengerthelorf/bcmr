---
cmd: bcmr stat
group: remote
sig: <host:path>
desc: show type and size of a remote file or directory
tags: [stable]
order: 2
related: [bcmr ls, bcmr du, bcmr hash]
flags:
  - { f: "<path>", t: "host:path | @bookmark", d: "—", x: "remote path to stat" }
example:
  - "lab:/data/dump.tar: file (15246928128 bytes, 14.2 GB)"
---

One round-trip type-and-size probe. `--json` emits
`{path, type, size}`.
