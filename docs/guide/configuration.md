# Configuration

BCMR reads configuration from `~/.config/bcmr/config.toml` (or `config.yaml`). All settings are optional — defaults are used when a key is absent.

## Full Example

```toml
[progress]
style = "fancy"          # "fancy" (default) or "plain" (same as --tui flag)

[progress.theme]
bar_gradient = ["#CABBE9", "#7E6EAC"]   # Hex color stops for the progress bar
bar_complete_char = "█"
bar_incomplete_char = "░"
text_color = "reset"                     # "reset", named color, or "#RRGGBB"
border_color = "#9E8BCA"
title_color = "#9E8BCA"

[progress.layout]
box_style = "rounded"    # "rounded" (default), "double", "heavy", "single"

[copy]
reflink = "auto"         # "auto" (default), "force", or "disable"
sparse = "auto"          # "auto" (default), "force", or "disable"

update_check = "notify"  # "notify" (default), "quiet", or "off"

[scp]
parallel_transfers = 4   # concurrent SSH transfers (default: 4)
compression = "auto"     # "auto" (default), "force", or "off"
```

## Progress Settings

### `progress.style`

| Value | Description |
|-------|-------------|
| `"fancy"` | TUI box with gradient bar, ETA, speed, per-file bar (default) |
| `"plain"` | 3-line text output, no box drawing |

### `progress.theme`

- **`bar_gradient`** — Array of hex colors. The progress bar interpolates between them. Default: `["#CABBE9", "#7E6EAC"]` (Morandi purple).
- **`bar_complete_char`** / **`bar_incomplete_char`** — Characters for filled and empty portions.
- **`text_color`** — Named color (`"red"`, `"green"`, etc.), hex (`"#RRGGBB"`), or `"reset"` for terminal default.
- **`border_color`** / **`title_color`** — Same format as `text_color`.

### `progress.layout.box_style`

| Value | Preview |
|-------|---------|
| `"rounded"` | `╭──╮ ╰──╯` |
| `"single"` | `┌──┐ └──┘` |
| `"double"` | `╔══╗ ╚══╝` |
| `"heavy"` | `┏━━┓ ┗━━┛` |

## Copy Settings

### `copy.reflink`

Controls copy-on-write (reflink) behavior. Can be overridden per-command with `--reflink`.

| Value | Description |
|-------|-------------|
| `"auto"` | Try reflink, fall back to regular copy (default) |
| `"force"` | Require reflink; fail if unsupported |
| `"disable"` | Never attempt reflink |

> **Note:** The config file also accepts `"never"` as an alias for `"disable"`.

### `copy.sparse`

Controls sparse file detection. Can be overridden per-command with `--sparse`.

| Value | Description |
|-------|-------------|
| `"auto"` | Detect zero blocks ≥ 4KB and create holes (default) |
| `"force"` | Always write sparse output, even for non-sparse sources |
| `"disable"` | Write all data, no hole detection |

> **Note:** The config file also accepts `"never"` as an alias for `"disable"`.

## SCP Settings

### `scp.parallel_transfers`

Number of concurrent SSH file transfers for remote copy. Can be overridden per-command with `-P`.

| Value | Description |
|-------|-------------|
| `4` | Default — 4 parallel SSH streams |
| `1` | Sequential transfer (no parallelism) |
| `N` | Any positive integer |

### `scp.compression`

Controls SSH transport compression for remote transfers.

| Value | Description |
|-------|-------------|
| `"auto"` | Smart: enable if >30% of bytes are compressible by extension (default) |
| `"force"` | Always enable SSH compression (`-o Compression=yes`) |
| `"off"` | Never compress |

In `auto` mode, known compressed extensions (`.gz`, `.zip`, `.mp4`, `.jpg`, etc.) are treated as incompressible. Compression is enabled only when a significant portion of the data would benefit.

## Update Check

Controls whether BCMR checks for new versions in the background when running any command.

| Value | Description |
|-------|-------------|
| `"notify"` | Check and print update notification to stderr (default) |
| `"quiet"` | No notification |
| `"off"` | Skip update check entirely |

## Config File Locations

BCMR checks these paths in order:

1. `~/.config/bcmr/config.toml`
2. `~/.config/bcmr/config.yaml`
3. Platform-specific config directory (via `directories` crate):
   - **macOS:** `~/Library/Application Support/com.bcmr.bcmr/`
   - **Windows:** `%APPDATA%\bcmr\bcmr\`
