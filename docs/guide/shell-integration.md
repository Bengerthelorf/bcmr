# Shell Integration

BCMR provides shell integration similar to zoxide. You can create aliases with custom prefixes, suffixes, or replace native commands entirely.

## Setup

Add one of the following to your shell configuration file:

::: code-group

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

## Shell Completions

BCMR provides tab completion for all commands and flags via `bcmr completions`.

::: code-group

```bash [Zsh]
# Add to ~/.zshrc
eval "$(bcmr completions zsh)"

# Or generate to fpath (faster startup)
bcmr completions zsh > ~/.zfunc/_bcmr
# Then ensure ~/.zshrc has: fpath=(~/.zfunc $fpath)
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

After setup, you can tab-complete commands and flags:

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```
