# 配置

BCMR 从 `~/.config/bcmr/config.toml`（或 `config.yaml`）读取配置。所有设置均为可选 — 缺省时使用默认值。

## 完整示例

```toml
[progress]
style = "fancy"          # "fancy"（默认）或 "plain"（与 --tui 参数相同）

[progress.theme]
bar_gradient = ["#CABBE9", "#7E6EAC"]   # 进度条的十六进制渐变色
bar_complete_char = "█"
bar_incomplete_char = "░"
text_color = "reset"                     # "reset"、颜色名或 "#RRGGBB"
border_color = "#9E8BCA"
title_color = "#9E8BCA"

[progress.layout]
box_style = "rounded"    # "rounded"（默认）、"double"、"heavy"、"single"

[copy]
reflink = "auto"         # "auto"（默认）、"force" 或 "disable"
sparse = "auto"          # "auto"（默认）、"force" 或 "disable"

update_check = "notify"  # "notify"（默认）、"quiet" 或 "off"

[scp]
parallel_transfers = 4   # 并行 SSH 传输数（默认：4）
compression = "auto"     # "auto"（默认）、"force" 或 "off"
```

## 进度设置

### `progress.style`

| 值 | 说明 |
|----|------|
| `"fancy"` | 带渐变进度条、ETA、速度和逐文件进度条的 TUI 界面（默认） |
| `"plain"` | 3 行文本输出，无边框绘制 |

### `progress.theme`

- **`bar_gradient`** — 十六进制颜色数组，进度条在颜色间插值。默认：`["#CABBE9", "#7E6EAC"]`（莫兰迪紫）。
- **`bar_complete_char`** / **`bar_incomplete_char`** — 已完成和未完成部分的字符。
- **`text_color`** — 颜色名（`"red"`、`"green"` 等）、十六进制（`"#RRGGBB"`）或 `"reset"` 使用终端默认色。
- **`border_color`** / **`title_color`** — 格式同 `text_color`。

### `progress.layout.box_style`

| 值 | 预览 |
|----|------|
| `"rounded"` | `╭──╮ ╰──╯` |
| `"single"` | `┌──┐ └──┘` |
| `"double"` | `╔══╗ ╚══╝` |
| `"heavy"` | `┏━━┓ ┗━━┛` |

## 复制设置

### `copy.reflink`

控制写时复制（reflink）行为。可通过 `--reflink` 参数逐命令覆盖。

| 值 | 说明 |
|----|------|
| `"auto"` | 尝试 reflink，失败则回退到常规复制（默认） |
| `"force"` | 要求使用 reflink；不支持时报错 |
| `"disable"` | 从不尝试 reflink |

> **注意：** 配置文件中也接受 `"never"` 作为 `"disable"` 的别名。

### `copy.sparse`

控制稀疏文件检测。可通过 `--sparse` 参数逐命令覆盖。

| 值 | 说明 |
|----|------|
| `"auto"` | 检测 ≥ 4KB 的零块并创建空洞（默认） |
| `"force"` | 始终写入稀疏输出，即使源文件非稀疏 |
| `"disable"` | 写入所有数据，不检测空洞 |

> **注意：** 配置文件中也接受 `"never"` 作为 `"disable"` 的别名。

## SCP 设置

### `scp.parallel_transfers`

远程复制时的并行 SSH 传输数。可通过 `-P` 参数覆盖。

| 值 | 说明 |
|----|------|
| `4` | 默认 — 4 个并行 SSH 流 |
| `1` | 顺序传输（无并行） |
| `N` | 任意正整数 |

### `scp.compression`

控制远程传输时的 SSH 传输层压缩。

| 值 | 说明 |
|----|------|
| `"auto"` | 智能模式：可压缩字节 >30% 时启用（默认） |
| `"force"` | 始终启用 SSH 压缩（`-o Compression=yes`） |
| `"off"` | 不压缩 |

`auto` 模式下，已知压缩格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被视为不可压缩。仅当大部分数据可受益于压缩时才启用。

## 更新检查

控制 BCMR 是否在每次运行命令时后台检查新版本。

| 值 | 说明 |
|----|------|
| `"notify"` | 检查并在 stderr 输出更新提示（默认） |
| `"quiet"` | 不输出提示 |
| `"off"` | 完全跳过更新检查 |

## 配置文件位置

BCMR 按以下顺序查找配置文件：

1. `~/.config/bcmr/config.toml`
2. `~/.config/bcmr/config.yaml`
3. 平台特定的配置目录（通过 `directories` crate）：
   - **macOS：** `~/Library/Application Support/com.bcmr.bcmr/`
   - **Windows：** `%APPDATA%\bcmr\bcmr\`
