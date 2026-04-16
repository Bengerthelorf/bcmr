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

BCMR 有**兩套獨立**的壓縮層，請區分：

### SSH 層（傳統 SCP 路徑）

遠端沒有安裝 bcmr，傳輸回退到 SCP 時，SSH 傳輸層可以用 zlib 壓縮。在 `[scp]` 中設定：

| 值 | 行為 |
|----|------|
| `"auto"` | 按副檔名判斷，可壓縮位元組 >30% 時啟用壓縮（預設） |
| `"force"` | 始終啟用 SSH 壓縮 |
| `"off"` | 不壓縮 |

`auto` 模式下，已知壓縮格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被視為不可壓縮。當大部分資料已經是壓縮格式時，跳過壓縮以避免 CPU 開銷。

### 線路層（serve 協定，`--compress`）

雙方都能說 [serve 協定](#serve-協定加速傳輸) 時，握手階段協商出每塊用 LZ4 還是 Zstd 壓縮。這是**獨立於且快於** SSH 內建的 zlib — Zstd-3 在類原始碼文字上達到 ~320 MB/s 編碼、~5× 縮減，zlib 的吞吐只有它的約 1/10。

| `--compress` 模式 | 客戶端通告的 caps | 協商結果 |
|---|---|---|
| `auto`（預設） | LZ4 + Zstd | 雙方都有 → Zstd-3；否則 LZ4；都沒有 → 原始 |
| `zstd` | 僅 Zstd | 伺服端也有 → Zstd-3；否則原始 |
| `lz4` | 僅 LZ4 | 伺服端也有 → LZ4；否則原始 |
| `none`/`off` | 無 | 只發原始 `Data` 幀 |

每個 4 MiB 區塊會自動判斷是否跳過壓縮 — 編碼後體積超過原始 95% 就直接發原始資料，這樣已經壓縮過的檔案（`.jpg`、`.zst`、`/dev/urandom`）開啟壓縮幾乎沒有代價。實際鏈路上的壓縮比和吞吐請見 [Wire Protocol 消融實驗](/ablation/wire-protocol#experiment-12-wire-compression-across-real-hosts)。

## Serve 協定（加速傳輸）

當遠端也安裝了 bcmr 時，傳輸自動使用 **bcmr serve 協定** — 透過單一 SSH 連線的二進位幀協定。消除逐檔 SSH 程序開銷，並支援伺服端雜湊計算。

遠端沒有 bcmr 時自動回退到傳統 SCP。

### 在遠端安裝 bcmr

```bash
# 部署 bcmr 到遠端主機
bcmr deploy user@host

# 自訂安裝路徑
bcmr deploy user@host --path /usr/local/bin/bcmr
```

`bcmr deploy` 自動偵測遠端 OS 與架構。相同平台時直接傳輸本機二進位檔，不同平台時從 GitHub Releases 下載對應版本。

### Serve 協定優勢

| | 傳統 SSH | Serve 協定 |
|---|---|---|
| 連線建立 | 每個檔案一個程序 | 單一持久連線 |
| 檔案列表 | `ssh find`（shell 解析） | 二進位 LIST 訊息 |
| 雜湊校驗 | 需傳回資料到本機 | 伺服端直接計算 BLAKE3 |
| 上傳校驗 | 需重新下載校驗 | 伺服端回傳寫入後的雜湊 |
| 單檔開銷 | ~50ms（程序啟動） | ~0.1ms（訊息幀） |

### 校驗遠端傳輸

```bash
# 上傳並校驗完整性
bcmr copy -V local_file.txt user@host:/backup/
```

### 內容定址去重（`CAP_DEDUP`）

serve 協定內建區塊級去重：≥ 16 MiB 的 PUT 會先交換每個 4 MiB 區塊的 BLAKE3 雜湊，伺服端只索取那些不在本機內容定址儲存（CAS）裡的區塊。CAS 路徑由 [`BCMR_CAS_DIR` / `BCMR_CAS_CAP_MB`](/zh-Hant/guide/configuration#環境變數) 控制。

對每個夠大的檔案 PUT 自動生效 — 無需額外旗標啟用。收益在「同一個建置產物反覆上傳」（開發迴圈）這種場景最明顯：第二次上傳略過伺服端已經有的所有區塊。

```bash
# 第一次：整個 64 MiB 走線路
bcmr copy build/artifact.bin user@host:/deploy/

# 第二次：每個區塊都命中 CAS，實際線路位元組接近 0
bcmr copy build/artifact.bin user@host:/deploy/alt-name.bin
```

協定時序見 [去重實驗](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put)。

### 快速模式（`--fast`）

以「伺服端省 CPU」換取略過伺服端 BLAKE3：

```bash
# 伺服端 Ok 回應中 hash 為 None。
bcmr copy --fast user@host:/big.bin ./local.bin

# 跟 -V 組合：客戶端重新 hash 目標檔案
bcmr copy --fast -V user@host:/big.bin ./local.bin
```

伺服端是 Linux 且 `--compress=none` 同時生效時，`--fast` 額外啟用 `splice(2)` 做 file → stdout 零拷貝。splice 實作 [目前並非所有場景都更快](/ablation/wire-protocol#experiment-14-cap-fast-real-numbers)；`--fast` 誠實記錄了這一取捨，詳情見 Internals。

預設關閉 — `--fast` 是明確放棄伺服端完整性校驗。

## 運作原理

- 使用現有的 SSH 設定（`~/.ssh/config`、金鑰等）
- 在開始傳輸前驗證 SSH 連線
- **Serve 模式**：透過 SSH 啟動遠端 `bcmr serve`，透過 stdin/stdout 的二進位協定通訊
- **傳統模式**：透過 ControlMaster 複用 SSH 連線，並行 worker 使用獨立 TCP 連線
- 透過 SSH 串流傳輸資料並追蹤進度
- 支援上傳和下載兩個方向

::: warning 限制
- 無法直接在兩個遠端主機之間複製 — 請使用本機作為中轉
- Serve 協定暫不支援斷點續傳（`-C`），需續傳時使用傳統模式（`-P 1`）
:::

## 路徑偵測

BCMR 透過 `[user@]host:path` 格式偵測遠端路徑。以下模式被識別為本機路徑，不會觸發遠端模式：

- 絕對路徑（`/path/to/file`）
- 相對路徑（`./file`、`../file`）
- 主目錄（`~/file`）
- Windows 磁碟機代號（`C:\file`）
