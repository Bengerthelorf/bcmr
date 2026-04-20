---
cmd: bcmr deploy
group: system
sig: <target> [--path <path>]
desc: install bcmr on a remote host (same-platform or cross-platform via github releases)
tags: [stable]
order: 2
related: [bcmr update]
flags:
  - { f: "<target>",  t: "user@host",      d: "—",                    x: "remote ssh target" }
  - { f: "--path",    t: path,             d: "~/.local/bin/bcmr",    x: "installation path on remote" }
example:
  - "probing remote ……  linux/x86_64"
  - "same arch · uploading local binary"
  - "bcmr 0.9.1 installed at ~/.local/bin/bcmr"
---

Enables the [serve protocol](/guide/remote-copy#serve-protocol) for direct-tcp
fast-path transfers. Remote platform is auto-detected; mismatched hosts
pull the matching binary from GitHub releases instead.
