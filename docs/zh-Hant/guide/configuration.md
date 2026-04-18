# 設定

BCMR 從 `~/.config/bcmr/config.toml`（或 `config.yaml`）讀取設定。所有設定均為可選 — 缺省時使用預設值。

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
reflink = "auto"         # "auto"（預設）、"force" 或 "disable"
sparse = "auto"          # "auto"（預設）、"force" 或 "disable"

update_check = "off"     # "off"（預設，不存取網路）、"quiet" 或 "notify"

[scp]
parallel_transfers = 4   # 並行 SSH 傳輸數（預設：4）
compression = "auto"     # "auto"（預設）、"force" 或 "off"
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
| `"force"` | 要求使用 reflink；不支援時報錯 |
| `"disable"` | 從不嘗試 reflink |

> **注意：** 設定檔中也接受 `"never"` 作為 `"disable"` 的別名。

### `copy.sparse`

控制稀疏檔案偵測。可透過 `--sparse` 參數逐命令覆寫。

| 值 | 說明 |
|----|------|
| `"auto"` | 偵測 ≥ 4KB 的零區塊並建立空洞（預設） |
| `"force"` | 始終寫入稀疏輸出，即使來源檔案非稀疏 |
| `"disable"` | 寫入所有資料，不偵測空洞 |

> **注意：** 設定檔中也接受 `"never"` 作為 `"disable"` 的別名。

## SCP 設定

### `scp.parallel_transfers`

遠端複製時的並行 SSH 傳輸數。可透過 `-P` 參數覆寫。

| 值 | 說明 |
|----|------|
| `4` | 預設 — 4 個並行 SSH 串流 |
| `1` | 順序傳輸（無並行） |
| `N` | 任意正整數 |

### `scp.compression`

控制遠端傳輸時的 SSH 傳輸層壓縮。

| 值 | 說明 |
|----|------|
| `"auto"` | 智慧模式：可壓縮位元組 >30% 時啟用（預設） |
| `"force"` | 始終啟用 SSH 壓縮（`-o Compression=yes`） |
| `"off"` | 不壓縮 |

`auto` 模式下，已知壓縮格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被視為不可壓縮。僅當大部分資料可受益於壓縮時才啟用。

## 更新檢查

控制 BCMR 是否在每次執行命令時於背景檢查新版本。

| 值 | 說明 |
|----|------|
| `"notify"` | 檢查並在 stderr 輸出更新提示（預設） |
| `"quiet"` | 不輸出提示 |
| `"off"` | 完全跳過更新檢查 |

## 設定檔位置

BCMR 按以下順序查找設定檔：

1. `~/.config/bcmr/config.toml`
2. `~/.config/bcmr/config.yaml`
3. 平台特定的設定目錄（透過 `directories` crate）：
   - **macOS：** `~/Library/Application Support/com.bcmr.bcmr/`
   - **Windows：** `%APPDATA%\bcmr\bcmr\`

## 環境變數

| 變數 | 預設值 | 說明 |
|------|--------|------|
| `BCMR_CAS_DIR` | `$XDG_DATA_HOME/bcmr/cas` | 覆蓋 [遠端去重](/zh-Hant/guide/remote-copy#wire-compression-dedup) 使用的內容定址儲存位置。整合測試也用它指向 tempdir 做隔離。 |
| `BCMR_CAS_CAP_MB` | `1024`（1 GiB） | CAS 的軟上限（位元組數），每次啟用去重的 PUT 前透過 LRU 驅逐來維持。設為 `0` 停用上限，讓倉庫無限增長。值以 MiB 為單位。 |

**未設定 `BCMR_CAS_DIR` 時的 CAS 路徑**：
- Linux：`~/.local/share/bcmr/cas/`
- macOS：`~/Library/Application Support/bcmr/cas/`
- Windows：`%APPDATA%\bcmr\cas\`

區塊檔案放在兩級十六進位前綴下（`<aa>/<bb>/<rest>.blk`），這樣在常見 workload 下單目錄不會超過約 6.5w 筆。清理倉庫是安全的：`rm -rf` 掉 cas 目錄，下次啟用去重的 PUT 會從線路重建。
