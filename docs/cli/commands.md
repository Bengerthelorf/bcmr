# Command Reference

---

## copy

Copy files and directories.

```
bcmr copy [OPTIONS] <SOURCES>... <DESTINATION>
```

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Recursively copy directories |
| `-p`, `--preserve` | Preserve file attributes (permissions, timestamps) |
| `-f`, `--force` | Overwrite existing files |
| `-y`, `--yes` | Skip overwrite confirmation prompt |
| `-v`, `--verbose` | Explain what is being done |
| `-e`, `--exclude <PATTERN>` | Exclude paths matching regex |
| `-t`, `--tui` | Use plain text progress display |
| `-n`, `--dry-run` | Preview without making changes |
| `-V`, `--verify` | Verify file integrity after copy (BLAKE3) |
| `-C`, `--resume` | Resume interrupted copy (size + mtime check) |
| `-s`, `--strict` | Use strict BLAKE3 hash verification for resume |
| `-a`, `--append` | Append to existing file (size check only, ignores mtime) |
| `--sync` | Sync data to disk after copy (fsync) |
| `--reflink <MODE>` | Copy-on-write: `auto` (default), `force`, `disable` |
| `--sparse <MODE>` | Sparse file handling: `auto` (default), `force`, `disable` |

**Examples:**

```bash
# Copy a single file
bcmr copy document.txt backup/

# Copy multiple files (shell globbing)
bcmr copy *.txt *.md backup/

# Recursively copy a directory
bcmr copy -r projects/ backup/

# Copy with attribute preservation
bcmr copy -rp important_dir/ /backup/

# Force overwrite without prompting
bcmr copy -fy source.txt destination.txt

# Exclude patterns (regex)
bcmr copy -r --exclude='\.git' --exclude='\.tmp$' src/ dest/

# Copy with verification
bcmr copy --verify critical_data.db /backup/

# Resume an interrupted copy
bcmr copy -C large_file.iso /backup/

# Strict resume — hash-verified append
bcmr copy -s large_file.iso /backup/

# Remote copy via SSH
bcmr copy local_file.txt user@host:/remote/path/
bcmr copy user@host:/remote/file.txt ./local/
```

### Resume Modes

| Flag | Behavior |
|------|----------|
| `-C` (resume) | Compare mtime — if match, append from where it left off; if mismatch, overwrite |
| `-s` (strict) | Compare BLAKE3 partial hash — if match, append; if mismatch, overwrite |
| `-a` (append) | Always append if dst is smaller, skip if same size, overwrite if larger |

---

## move

Move files and directories.

```
bcmr move [OPTIONS] <SOURCES>... <DESTINATION>
```

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Recursively move directories |
| `-p`, `--preserve` | Preserve file attributes |
| `-f`, `--force` | Overwrite existing files |
| `-y`, `--yes` | Skip overwrite confirmation prompt |
| `-v`, `--verbose` | Explain what is being done |
| `-e`, `--exclude <PATTERN>` | Exclude paths matching regex |
| `-t`, `--tui` | Use plain text progress display |
| `-n`, `--dry-run` | Preview without making changes |
| `-V`, `--verify` | Verify file integrity after move |
| `-C`, `--resume` | Resume interrupted move (cross-device fallback only) |
| `-s`, `--strict` | Use strict hash verification for resume |
| `-a`, `--append` | Append mode for cross-device moves |
| `--sync` | Sync data to disk (cross-device only) |

**Examples:**

```bash
# Move a single file
bcmr move old_file.txt new_location/

# Move multiple files
bcmr move file1.txt file2.txt new_location/

# Recursively move a directory
bcmr move -r old_project/ new_location/

# Move with exclusions
bcmr move -r --exclude='^node_modules' --exclude='\.log$' project/ dest/

# Dry run
bcmr move -r -n old_project/ new_location/
```

::: tip
Same-device moves use `rename(2)` and complete instantly. Cross-device moves automatically fall back to copy + delete, with progress tracking.
:::

---

## remove

Remove files and directories.

```
bcmr remove [OPTIONS] <PATHS>...
```

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Recursively remove directories |
| `-f`, `--force` | Force removal without confirmation |
| `-y`, `--yes` | Skip confirmation prompt |
| `-i`, `--interactive` | Prompt before each removal |
| `-v`, `--verbose` | Explain what is being done |
| `-d`, `--dir` | Remove empty directories only (like `rmdir`) |
| `-e`, `--exclude <PATTERN>` | Exclude paths matching regex |
| `-t`, `--tui` | Use plain text progress display |
| `-n`, `--dry-run` | Preview without making changes |

**Examples:**

```bash
# Remove a single file
bcmr remove unnecessary.txt

# Remove multiple files (globbing)
bcmr remove *.log

# Recursively remove a directory
bcmr remove -r old_project/

# Interactive removal
bcmr remove -i file1.txt file2.txt file3.txt

# Remove with exclusions (keep .important and .backup files)
bcmr remove -r --exclude='\.important$' --exclude='\.backup$' trash/

# Dry run
bcmr remove -r -n potentially_important_folder/
```

---

## init

Generate shell integration scripts. See [Shell Integration](/guide/shell-integration) for details.

```
bcmr init <SHELL> [OPTIONS]
```

| Argument / Flag | Description |
|-----------------|-------------|
| `<SHELL>` | `bash`, `zsh`, or `fish` |
| `--cmd <prefix>` | Command prefix (e.g., `b` → `bcp`, `bmv`, `brm`) |
| `--prefix <prefix>` | Explicit prefix (overrides `--cmd`) |
| `--suffix <suffix>` | Command suffix |
| `--no-cmd` | Don't create aliases |
| `--path <path>` | Add directory to PATH |

**Examples:**

```bash
eval "$(bcmr init zsh --cmd b)"          # bcp, bmv, brm
eval "$(bcmr init bash --cmd '')"         # cp, mv, rm
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"  # pcp+, pmv+, prm+
```

---

## completions

Generate shell completion scripts. See [Shell Integration](/guide/shell-integration#shell-completions) for setup instructions.

```
bcmr completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

**Examples:**

```bash
bcmr completions zsh > ~/.zfunc/_bcmr
bcmr completions bash > /etc/bash_completion.d/bcmr
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
bcmr completions powershell >> $PROFILE
```

---

## update

Check for updates and self-update the binary from GitHub Releases.

```
bcmr update
```

Downloads the latest release for your platform and replaces the current binary in place.

BCMR also checks for updates in the background on every command run (configurable via `update_check` in [Configuration](/guide/configuration)).
