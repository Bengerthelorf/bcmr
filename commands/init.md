---
cmd: bcmr init
group: shell
sig: <shell> [options]
desc: print a shell integration script for cp/mv/rm functions and zsh completions
tags: [stable]
order: 1
related: [bcmr completions]
flags:
  - { f: "<shell>",      t: "bash|zsh|fish|powershell", d: "—", x: "target shell" }
  - { f: "--cmd [<pfx>]", t: str,            d: "empty", x: "wrapper prefix; b creates bcp, bmv, brm; empty creates cp, mv, rm" }
  - { f: "--prefix",     t: str,             d: "—", x: "explicit prefix; requires --cmd and overrides its value" }
  - { f: "--suffix",     t: str,             d: "—", x: "wrapper suffix; requires --cmd" }
  - { f: "--no-cmd",     t: bool,            d: "false", x: "skip command wrappers and zsh completion activation" }
  - { f: "--path",       t: path,            d: "—", x: "add directory to PATH" }
  - { f: "--config <PATH>", t: path,         d: "—", x: "ignored; shell setup does not load operation configuration" }
example:
  - "# add to .zshrc"
  - "eval \"$(bcmr init zsh --cmd b)\""
  - "# now: bcp, bmv, brm"
---

This command prints a script; evaluate or source that output to activate
the integration. Without `--cmd`, it defines `cp`, `mv`, and `rm` shell
functions that call bcmr. Use `--cmd b` to create `bcp`, `bmv`, and `brm`.
Zsh initialization also activates completion for bcmr and those wrappers.

Add the appropriate line to your shell configuration and run it in the
current shell:

```sh
# Zsh: ~/.zshrc
eval "$(bcmr init zsh --cmd b)"
# Bash: ~/.bashrc
eval "$(bcmr init bash --cmd b)"
```

```fish
# Fish: ~/.config/fish/config.fish
bcmr init fish --cmd b | source
```

```powershell
# PowerShell: $PROFILE
bcmr init powershell --cmd b | Out-String | Invoke-Expression
```

`--no-cmd` disables both wrapper definitions and zsh completion activation;
it can still be combined with `--path` to update PATH. See the
[shell integration guide](/guide/shell-integration) for the full walkthrough.
