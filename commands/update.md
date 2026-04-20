---
cmd: bcmr update
group: system
sig: ""
desc: check github releases and self-update the binary in place
tags: [stable]
order: 1
related: [bcmr deploy]
example:
  - "current: 0.9.0"
  - "latest:  0.9.1"
  - "downloading ……  ok"
  - "replaced /usr/local/bin/bcmr · all good"
---

bcmr also runs this check in the background once per invocation (rate
limited). Disable via `update_check = false` in
[config](/guide/configuration).
