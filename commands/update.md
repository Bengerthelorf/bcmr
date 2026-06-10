---
cmd: bcmr update
group: system
sig: "[--check]"
desc: check github releases and self-update the binary in place
tags: [stable]
order: 1
related: [bcmr deploy]
flags:
  - { f: "--check", t: bool, d: "false", x: "print current and latest versions without installing" }
example:
  - "current: 0.9.0"
  - "latest:  0.9.1"
  - "downloading ……  ok"
  - "replaced /usr/local/bin/bcmr · all good"
---

bcmr can also run this check in the background once per invocation (rate
limited). Off by default — opt in with `update_check = "notify"` in
[config](/guide/configuration).
