---
cmd: bcmr completions
group: shell
sig: <shell> [--install [--print | --uninstall]]
desc: generate shell completion script for bash, zsh, fish, powershell, or elvish
tags: [stable]
order: 2
related: [bcmr init]
flags:
  - { f: "<shell>", t: "bash|zsh|fish|powershell|elvish", d: "—", x: "target shell" }
  - { f: "--install", t: bool, d: "false", x: "write to the conventional shell path; unsupported for PowerShell" }
  - { f: "--print", t: bool, d: "false", x: "with --install, print the target path without writing" }
  - { f: "--uninstall", t: bool, d: "false", x: "with --install, remove the installed file; conflicts with --print" }
  - { f: "--config <PATH>", t: path, d: "—", x: "ignored; completion generation does not load operation configuration" }
example:
  - "mkdir -p ~/.zfunc"
  - "bcmr completions zsh > ~/.zfunc/_bcmr"
  - "fpath=(~/.zfunc $fpath)"
  - "autoload -Uz compinit && compinit"
---

Without `--install`, the script is written to stdout. Installing a file
does not activate it in the current shell. For zsh, the quickest activation
of bcmr and `bcp`/`bmv`/`brm` completion is:

```sh
eval "$(bcmr init zsh --cmd b)"
```

To use the installed file through zsh's `fpath`, add these lines before any
existing `compinit` call in `~/.zshrc`, and run them in the current shell:

```zsh
bcmr completions zsh --install
completion_file="$(bcmr completions zsh --install --print)"
fpath=("${completion_file:h}" $fpath)
autoload -Uz compinit && compinit
```

`--install --print` only reports the path; it creates no file. Remove a
file with `bcmr completions zsh --install --uninstall`. Open a new shell to
clear already loaded file-based completions. Initialization in `~/.zshrc`
continues to activate completions until that initialization line is removed.

Default install paths (absolute `XDG_DATA_HOME` / `XDG_CONFIG_HOME` values
override their respective defaults):

| Shell | Path |
| --- | --- |
| Bash | `~/.local/share/bash-completion/completions/bcmr` |
| Zsh | `~/.local/share/zsh/site-functions/_bcmr` |
| Fish | `~/.config/fish/completions/bcmr.fish` |
| Elvish | `~/.config/elvish/lib/bcmr.elv` |

Bash needs the bash-completion package loaded by the shell. PowerShell
does not support automatic installation; append the output of
`bcmr completions powershell` to `$PROFILE`.
