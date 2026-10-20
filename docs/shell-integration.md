---
title: Shell Integration
section: guide
order: 2
---

BCMR provides shell integration similar to zoxide. You can create aliases with custom prefixes, suffixes, or replace native commands entirely.

## Setup

Add one of the following to your shell configuration file:

:::code-group

```bash [Zsh (~/.zshrc)]
# Use 'b' prefix → bcp, bmv, brm
eval "$(bcmr init zsh --cmd b)"
```

```bash [Bash (~/.bashrc)]
# Use 'b' prefix → bcp, bmv, brm
eval "$(bcmr init bash --cmd b)"
```

```fish [Fish (~/.config/fish/config.fish)]
# Use 'b' prefix → bcp, bmv, brm
bcmr init fish --cmd b | source
```

```powershell [PowerShell ($PROFILE)]
# Use 'b' prefix → bcp, bmv, brm
bcmr init powershell --cmd b | Out-String | Invoke-Expression
```

:::

## Options

| Flag | Description |
|------|-------------|
| `--cmd <prefix>` | Set command prefix (e.g., `b` creates `bcp`, `bmv`, `brm`) |
| `--prefix <prefix>` | Explicitly set prefix (overrides `--cmd`) |
| `--suffix <suffix>` | Set command suffix |
| `--no-cmd` | Don't create command aliases |
| `--path <path>` | Add a directory to PATH |

## Examples

```bash
# Replace native commands (creates cp, mv, rm)
eval "$(bcmr init zsh --cmd '')"

# Custom prefix (creates testcp, testmv, testrm)
eval "$(bcmr init zsh --cmd test)"

# Prefix + suffix (creates pcp+, pmv+, prm+)
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"

# Only add to PATH, no aliases
eval "$(bcmr init zsh --no-cmd --path /usr/local/bin)"
```

## Supported Shells

- Bash
- Zsh
- Fish
- PowerShell

## Shell Completions

BCMR provides tab completion for all commands and flags via `bcmr completions`.

:::code-group

```bash [Zsh]
# Add to ~/.zshrc; this also initializes zsh's completion system when needed.
eval "$(bcmr init zsh --cmd b)"

# Optional: install a generated completion file for loading through fpath.
bcmr completions zsh --install
# Print its resolved location without writing it:
bcmr completions zsh --install --print
```

```bash [Bash]
# Add to ~/.bashrc
eval "$(bcmr completions bash)"

# Or generate to system completions dir
bcmr completions bash > /etc/bash_completion.d/bcmr
```

```fish [Fish]
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
```

```powershell [PowerShell]
# Create profile directory if it doesn't exist, then append
New-Item -Path (Split-Path $PROFILE) -ItemType Directory -Force | Out-Null
bcmr completions powershell >> $PROFILE

# Or load for current session only
bcmr completions powershell | Out-String | Invoke-Expression
```

:::

`bcmr init zsh --cmd b` creates `bcp`, `bmv`, and `brm`, initializes `compinit` only if needed, and loads both local and remote completions. It is safe to run again. Keep any existing zsh completion setup before this line. `--no-cmd` retains its PATH-only behavior and does not initialize completions.

`--install` only writes a file; it does not activate completion in an existing shell or edit your configuration. The command prints the resolved installation directory (including the default when `XDG_DATA_HOME` is unset). For file-based loading, add that directory to `fpath` before `autoload -Uz compinit; compinit`. The self-contained `init` line above is sufficient for command wrappers and completions.

If `compinit` reports insecure directories, inspect them with `compaudit` and correct their ownership or permissions before retrying. Initialization preserves these checks and reports completion failures while leaving command wrappers available.

After setup, you can tab-complete commands and flags:

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```

:::callout[Alias Completions (Zsh)]{kind="info"}
When using `bcmr init zsh --cmd <prefix>`, one line enables command wrappers and their completions:

```bash
eval "$(bcmr init zsh --cmd b)"
```

Then you can tab-complete the commands directly:

```
bcp -<TAB>   → --recursive --preserve --force --verify ...
bmv -<TAB>   → --recursive --preserve --force --verify ...
```
:::

Remote completion normally stays quiet when a host is unavailable. To inspect a failure, run the hidden completion command directly, or enable diagnostics for Tab completion:

```bash
bcmr __complete-remote 'user@example.com:/path/'
export BCMR_COMPLETION_DEBUG=1
# Disable diagnostic output again:
unset BCMR_COMPLETION_DEBUG
```
