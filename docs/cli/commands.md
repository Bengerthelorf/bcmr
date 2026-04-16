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
| `-P`, `--parallel <N>` | Parallel SSH transfers for remote copy (default: config value) |
| `-j`, `--jobs <N>` | Max concurrent local file copies (default: `min(CPU, 8)`) |
| `--compress <MODE>` | Remote-wire compression: `auto` (default, LZ4+Zstd advertised), `zstd`, `lz4`, `none` |
| `--fast` | Remote only: skip server-side BLAKE3 (`Ok { hash: None }`); on Linux server also use `splice(2)`. Use only if you verify integrity another way (e.g. `-V` re-hashes on the client). |

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

# Parallel remote upload (4 workers)
bcmr copy -P 4 file1.bin file2.bin user@host:/remote/
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
| `-j`, `--jobs <N>` | Max concurrent file copies when move falls back to copy+delete (default: `min(CPU, 8)`) |
| `--compress <MODE>` | Wire compression for remote moves: `auto`, `zstd`, `lz4`, `none` |
| `--fast` | Remote only: skip server-side BLAKE3 on the copy phase of cross-device moves |

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

---

## deploy

Deploy bcmr to a remote host for [serve protocol](/guide/remote-copy#serve-protocol-accelerated-transfers) support.

```
bcmr deploy <TARGET> [--path <PATH>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<TARGET>` | required | Remote target (`user@host`) |
| `--path` | `~/.local/bin/bcmr` | Installation path on remote |

Detects remote OS and architecture automatically. Same platform: transfers local binary. Cross-platform: downloads from GitHub Releases.

```bash
bcmr deploy user@server
bcmr deploy root@10.0.0.1 --path /usr/local/bin/bcmr
```

---

## check

Compare source and destination without making changes. Reports files that are added, modified, or missing.

```
bcmr check [OPTIONS] <SOURCES>... <DESTINATION>
```

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Recursively compare directories |
| `-e`, `--exclude <PATTERN>` | Exclude paths matching regex |

**Exit codes:** `0` = in sync, `1` = differences found, `2` = error.

Comparison uses file size and modification time — no content hashing.

**Examples:**

```bash
# Compare two directories
bcmr check -r src/ backup/

# Check with JSON output (for scripts / AI agents)
bcmr check --json -r src/ backup/
```

---

## status

Inspect the state of `--json` background jobs (v0.5.4+).

```
bcmr status [JOB_ID] [--json]
```

When `bcmr copy`, `move`, or `remove` is invoked with `--json`, the
command detaches to a background process and writes progress as
NDJSON to `~/.local/share/bcmr/jobs/<id>.jsonl`. `bcmr status`
reads that log and classifies the job into one of five states:

| State | Meaning |
|-------|---------|
| `scanning` | alive, still walking the source tree or waiting for the first progress event |
| `running` | alive, in the transfer phase |
| `done` | finished with `status:"success"` |
| `failed` | finished with `status:"error"` (error message preserved in the log) |
| `interrupted` | pid no longer alive but no terminal event was written (crash / SIGKILL / OOM) |

**Examples:**

```bash
# List all recent jobs (tab-separated: id, state, last %)
bcmr status

# Detail on one job (prints the latest log line as JSON)
bcmr status 764dee358ff4

# Structured wrapper for scripts: { job_id, state, latest }
bcmr status 764dee358ff4 --json
```

Logs older than 7 days are cleaned up automatically when a new
job starts.

---

## Global Flags

These flags apply to all commands:

| Flag | Description |
|------|-------------|
| `--json` | Output NDJSON streaming progress and structured results (for AI agents and scripts) |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |

### JSON Output

When `--json` is passed to `copy`, `move`, or `remove`, bcmr
**detaches to a background process** and prints one descriptor
line on stdout:

```json
{"job_id":"764dee358ff4","pid":36897,"log":"/home/.../jobs/764dee358ff4.jsonl"}
```

The parent exits immediately. Progress events, errors, and the
final result are appended to the log file; poll it with
[`bcmr status <job_id>`](#status). The foreground `--json`
behaviour is preserved for `check` (no detach — there's nothing
long-running to poll).

**Progress lines** (emitted every ~200ms during transfers):

```json
{"type":"progress","operation":"Copying","bytes_done":1048576,"bytes_total":10485760,"percent":10.0,"speed_bps":52428800,"eta_secs":2,"file":"large.bin","file_size":10485760,"file_progress":1048576,"scanning":false}
```

The `"scanning":true` variant is emitted as a heartbeat while the
plan phase walks the source tree; consumers can use it to prove
the job is alive even before any byte has moved.

**Result line** (emitted on completion, success or failure):

```json
{"type":"result","status":"success","operation":"Copying","bytes_total":10485760,"duration_secs":3.2,"avg_speed_bps":3276800}
```

```json
{"type":"result","status":"error","operation":"Removing","bytes_total":0,"duration_secs":0.001,"error":"Cannot remove '/tmp/x': Is a directory (use -r for recursive removal)"}
```

**Check output** (`bcmr check --json`, foreground):

```json
{"command":"check","status":"success","in_sync":false,"added":[{"path":"new.txt","size":4096,"src_size":4096,"is_dir":false}],"modified":[],"missing":[],"summary":{"added":1,"modified":0,"missing":0,"total_bytes":4096}}
```

Interactive prompts are auto-accepted in JSON mode (`--json` implies `--yes`).
