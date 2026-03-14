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

## 運作原理

- 使用現有的 SSH 設定（`~/.ssh/config`、金鑰等）
- 在開始傳輸前驗證 SSH 連線
- 透過 SSH 串流傳輸資料並追蹤進度
- 支援上傳和下載兩個方向

::: warning 限制
- 無法直接在兩個遠端主機之間複製 — 請使用本機作為中轉
- SSH 必須設定為非交互式（基於金鑰的）認證（`BatchMode=yes`）
- 遠端傳輸不支援排除規則和進階複製功能（reflink、sparse、resume）
:::

## 路徑偵測

BCMR 透過 `[user@]host:path` 格式偵測遠端路徑。以下模式被識別為本機路徑，不會觸發遠端模式：

- 絕對路徑（`/path/to/file`）
- 相對路徑（`./file`、`../file`）
- 主目錄（`~/file`）
- Windows 磁碟機代號（`C:\file`）
