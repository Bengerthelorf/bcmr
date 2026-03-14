# 設定

BCMR 從 `~/.config/bcmr/config.toml` 讀取設定。所有設定均為可選 — 缺省時使用預設值。

## 完整範例

```toml
[progress]
style = "fancy"          # "fancy"（預設）或 "plain"（與 --tui 參數相同）

[progress.theme]
bar_gradient = ["#CABBE9", "#7E6EAC"]   # 進度條的十六進位漸變色
bar_complete_char = "█"
bar_incomplete_char = "░"
text_color = "reset"                     # "reset"、顏色名或 "#RRGGBB"
border_color = "#9E8BCA"
title_color = "#9E8BCA"

[progress.layout]
box_style = "rounded"    # "rounded"（預設）、"double"、"heavy"、"single"

[copy]
reflink = "auto"         # "auto"（預設）或 "never"
sparse = "auto"          # "auto"（預設）或 "never"

update_check = "notify"  # "notify"（預設）、"quiet" 或 "off"
```

## 進度設定

### `progress.style`

| 值 | 說明 |
|----|------|
| `"fancy"` | 帶漸變進度條、ETA、速度和逐檔案進度條的 TUI 介面（預設） |
| `"plain"` | 3 行文字輸出，無邊框繪製 |

### `progress.theme`

- **`bar_gradient`** — 十六進位顏色陣列，進度條在顏色間插值。預設：`["#CABBE9", "#7E6EAC"]`（莫蘭迪紫）。
- **`bar_complete_char`** / **`bar_incomplete_char`** — 已完成和未完成部分的字元。
- **`text_color`** — 顏色名（`"red"`、`"green"` 等）、十六進位（`"#RRGGBB"`）或 `"reset"` 使用終端預設色。
- **`border_color`** / **`title_color`** — 格式同 `text_color`。

### `progress.layout.box_style`

| 值 | 預覽 |
|----|------|
| `"rounded"` | `╭──╮ ╰──╯` |
| `"single"` | `┌──┐ └──┘` |
| `"double"` | `╔══╗ ╚══╝` |
| `"heavy"` | `┏━━┓ ┗━━┛` |

## 複製設定

### `copy.reflink`

控制寫時複製（reflink）行為。可透過 `--reflink` 參數逐命令覆寫。

| 值 | 說明 |
|----|------|
| `"auto"` | 嘗試 reflink，失敗則回退到常規複製（預設） |
| `"never"` | 從不嘗試 reflink |

### `copy.sparse`

控制稀疏檔案偵測。可透過 `--sparse` 參數逐命令覆寫。

| 值 | 說明 |
|----|------|
| `"auto"` | 偵測 ≥ 4KB 的零區塊並建立空洞（預設） |
| `"never"` | 寫入所有資料，不偵測空洞 |

## 更新檢查

控制 BCMR 是否在每次執行命令時於背景檢查新版本。

| 值 | 說明 |
|----|------|
| `"notify"` | 檢查並在 stderr 輸出更新提示（預設） |
| `"quiet"` | 不輸出提示 |
| `"off"` | 完全跳過更新檢查 |
