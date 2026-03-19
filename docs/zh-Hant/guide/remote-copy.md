# 遠端複製 (SSH)

BCMR 支援使用 SCP 風格語法透過 SSH 複製檔案到遠端主機或從遠端主機複製檔案。

## 語法

```bash
# 上傳：本機 → 遠端
bcmr copy local_file.txt user@host:/remote/path/

# 下載：遠端 → 本機
bcmr copy user@host:/remote/file.txt ./local/

# 遞迴上傳
bcmr copy -r local_dir/ user@host:/remote/path/

# 遞迴下載
bcmr copy -r user@host:/remote/dir/ ./local/
```

## 並行傳輸

使用 `-P` 參數同時傳輸多個檔案：

```bash
# 4 個並行上傳
bcmr copy -P 4 file1.bin file2.bin file3.bin file4.bin user@host:/remote/

# 遞迴上傳，8 個 worker
bcmr copy -r -P 8 ./large_dataset/ user@host:/data/

# 並行下載
bcmr copy -P 3 user@host:/data/a.bin user@host:/data/b.bin ./local/
```

預設並行數透過 `[scp] parallel_transfers` 設定（預設：4）。使用 `-P 1` 或小量傳輸時按順序執行。

TUI 和純文字模式均會顯示每個 worker 的狀態：

```
Uploading: [████████░░░░░░░░░░░░░░░░░░] 42% [3/4w]
150 MiB / 350 MiB | 45.5 MiB/s | ETA: 04:32
[1] large.iso 53% | [2] backup.tar 78% | [3] data.csv 12% | [4] idle
```

## 壓縮

啟用 SSH 壓縮可在慢速鏈路上減少傳輸時間。在 `[scp]` 中設定：

| 值 | 行為 |
|----|------|
| `"auto"` | 按副檔名判斷，可壓縮位元組 >30% 時啟用壓縮（預設） |
| `"force"` | 始終啟用 SSH 壓縮 |
| `"off"` | 不壓縮 |

`auto` 模式下，已知壓縮格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被視為不可壓縮。當大部分資料已經是壓縮格式時，跳過壓縮以避免 CPU 開銷。

## 運作原理

- 使用現有的 SSH 設定（`~/.ssh/config`、金鑰等）
- 在開始傳輸前驗證 SSH 連線
- 透過 ControlMaster 複用 SSH 連線
- 透過 SSH 串流傳輸資料並追蹤進度
- 支援上傳和下載兩個方向

::: warning 限制
- 無法直接在兩個遠端主機之間複製 — 請使用本機作為中轉
- 遠端傳輸不支援排除規則和進階複製功能（reflink、sparse、resume）
:::

## 路徑偵測

BCMR 透過 `[user@]host:path` 格式偵測遠端路徑。以下模式被識別為本機路徑，不會觸發遠端模式：

- 絕對路徑（`/path/to/file`）
- 相對路徑（`./file`、`../file`）
- 主目錄（`~/file`）
- Windows 磁碟機代號（`C:\file`）
