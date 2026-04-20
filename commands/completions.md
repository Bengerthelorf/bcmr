---
cmd: bcmr completions
group: shell
sig: <shell>
desc: generate shell completion script for bash, zsh, fish, powershell, or elvish
tags: [stable]
order: 2
related: [bcmr init]
flags:
  - { f: "<shell>", t: "bash|zsh|fish|powershell|elvish", d: "—", x: "target shell" }
example:
  - "bcmr completions zsh > ~/.zfunc/_bcmr"
  - "autoload -Uz compinit && compinit"
---
