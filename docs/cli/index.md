# CLI Overview

BCMR provides three main commands for file operations, plus shell integration and self-update.

## Commands

| Command | Description |
|---------|-------------|
| [`copy`](/cli/commands#copy) | Copy files and directories |
| [`move`](/cli/commands#move) | Move files and directories |
| [`remove`](/cli/commands#remove) | Remove files and directories |
| [`init`](/cli/commands#init) | Generate shell integration script |
| [`update`](/cli/commands#update) | Check for updates and self-update |

## Common Flags

These flags are shared across `copy`, `move`, and `remove`:

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Operate on directories recursively |
| `-f`, `--force` | Overwrite existing files / force removal |
| `-y`, `--yes` | Skip confirmation prompts |
| `-v`, `--verbose` | Explain what is being done |
| `-e`, `--exclude <PATTERN>` | Exclude paths matching regex |
| `-t`, `--tui` | Use plain text progress display |
| `-n`, `--dry-run` | Preview operation without making changes |

## Dry Run

All commands that modify files accept `-n` / `--dry-run`. This shows a colored plan of what would happen:

```bash
bcmr copy -r -n projects/ backup/
bcmr move -n old_file.txt new_location/
bcmr remove -r -n old_project/
```

Actions are color-coded: <span style="color: green">ADD</span>, <span style="color: yellow">OVERWRITE</span>, <span style="color: blue">APPEND</span>, <span style="color: cyan">MOVE</span>, <span style="color: grey">SKIP</span>, <span style="color: red">REMOVE</span>.
