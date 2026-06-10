---
cmd: bcmr hash
group: remote
sig: <host:path>
desc: compute the blake3 hash of a remote file without downloading it
tags: [stable]
order: 4
related: [bcmr ls, bcmr stat, bcmr check]
flags:
  - { f: "<path>", t: "host:path | @bookmark", d: "—", x: "remote file to hash" }
example:
  - "af1349b9f5f9a1a6…  lab:/data/dump.tar"
---

The hash is computed remotely (server-side blake3 via the serve
protocol), so only the digest crosses the wire — handy for spot-checking
a transfer without re-downloading. `--json` emits `{path, hash, algo}`.
