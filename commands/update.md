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
  - { f: "--json", t: bool, d: "false", x: "emit a structured update check or installation result" }
  - { f: "--config <PATH>", t: path, d: "—", x: "layer this file over discovered configuration" }
example:
  - "bcmr update --check"
  - "Current version: 0.7.0"
  - "Latest version:  0.7.0 (up to date)"
---

For a Homebrew installation, upgrade through Homebrew:

```sh
brew update
brew upgrade Bengerthelorf/tap/bcmr
```

`bcmr update --check` remains available. Self-update refuses to replace a
Homebrew-managed binary.

bcmr can also run this check in the background on eligible commands.
Off by default — opt in with `update_check = "notify"` in
[config](/guide/configuration).
