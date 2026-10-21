---
cmd: bcmr du
group: remote
sig: <host:path>
desc: show recursive size of a remote path (du -sh equivalent)
tags: [stable]
order: 3
related: [bcmr ls, bcmr stat, bcmr hash]
flags:
  - { f: "<path>", t: "host:path | @bookmark", d: "—", x: "remote path to size" }
  - { f: "--json", t: bool, d: "false", x: "emit structured byte count and readable size" }
  - { f: "--config <PATH>", t: path, d: "—", x: "layer this file over discovered configuration" }
example:
  - "14.2 GB	lab:/data/projects"
---

Recursive size of a remote tree without a shell session. `--json` emits
`{path, bytes, human}`.
