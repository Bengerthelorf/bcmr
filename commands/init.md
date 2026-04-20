---
cmd: bcmr init
group: shell
sig: <shell> [options]
desc: generate shell integration script with alias prefix/suffix for cp/mv/rm replacements
tags: [stable]
order: 1
related: [bcmr completions]
flags:
  - { f: "<shell>",      t: "bash|zsh|fish", d: "—", x: "target shell" }
  - { f: "--cmd <pfx>",  t: str,             d: "—", x: "command prefix, e.g. b → bcp, bmv, brm" }
  - { f: "--prefix",     t: str,             d: "—", x: "explicit prefix (overrides --cmd)" }
  - { f: "--suffix",     t: str,             d: "—", x: "command suffix" }
  - { f: "--no-cmd",     t: bool,            d: "false", x: "don't create aliases" }
  - { f: "--path",       t: path,            d: "—", x: "add directory to PATH" }
example:
  - "# add to .zshrc"
  - "eval \"$(bcmr init zsh --cmd b)\""
  - "# now: bcp, bmv, brm"
---

Shell integration lets `bcmr` stand in for `cp` / `mv` / `rm` through
aliased prefixes so your muscle memory keeps working. See the
[shell integration guide](/guide/shell-integration) for the full walkthrough.
