# 命令參考

---

## copy

複製檔案和目錄。

```
bcmr copy [選項] <來源路徑>... <目標路徑>
```

| 參數 | 說明 |
|------|------|
| `-r`, `--recursive` | 遞迴複製目錄 |
| `-p`, `--preserve` | 保留檔案屬性（權限、時間戳記） |
| `-f`, `--force` | 覆寫已有檔案 |
| `-y`, `--yes` | 跳過覆寫確認提示 |
| `-v`, `--verbose` | 顯示詳細操作資訊 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正規表達式的路徑 |
| `-t`, `--tui` | 使用純文字進度顯示 |
| `-n`, `--dry-run` | 預覽但不執行 |
| `-V`, `--verify` | 複製後校驗檔案完整性 (BLAKE3) |
| `-C`, `--resume` | 斷點續傳（大小 + mtime 檢查） |
| `-s`, `--strict` | 嚴格 BLAKE3 雜湊校驗續傳 |
| `-a`, `--append` | 追加模式（僅檢查大小，忽略 mtime） |
| `--sync` | 複製後同步到磁碟 (fsync) |
| `--reflink <MODE>` | 寫時複製：`auto`（預設）、`force`、`disable` |
| `--sparse <MODE>` | 稀疏檔案：`auto`（預設）、`force`、`disable` |

**範例：**

```bash
# 複製單個檔案
bcmr copy document.txt backup/

# 複製多個檔案（shell 萬用字元）
bcmr copy *.txt *.md backup/

# 遞迴複製目錄
bcmr copy -r projects/ backup/

# 帶屬性保留的複製
bcmr copy -rp important_dir/ /backup/

# 強制覆寫且不提示
bcmr copy -fy source.txt destination.txt

# 排除模式（正規表達式）
bcmr copy -r --exclude='\.git' --exclude='\.tmp$' src/ dest/

# 帶校驗的複製
bcmr copy --verify critical_data.db /backup/

# 斷點續傳
bcmr copy -C large_file.iso /backup/

# SSH 遠端複製
bcmr copy local_file.txt user@host:/remote/path/
```

### 續傳模式

| 參數 | 行為 |
|------|------|
| `-C`（resume） | 比較 mtime — 匹配則從斷點追加；不匹配則覆寫 |
| `-s`（strict） | 比較 BLAKE3 部分雜湊 — 匹配則追加；不匹配則覆寫 |
| `-a`（append） | 目標較小則追加，大小相同則跳過，目標較大則覆寫 |

---

## move

移動檔案和目錄。

```
bcmr move [選項] <來源路徑>... <目標路徑>
```

| 參數 | 說明 |
|------|------|
| `-r`, `--recursive` | 遞迴移動目錄 |
| `-p`, `--preserve` | 保留檔案屬性 |
| `-f`, `--force` | 覆寫已有檔案 |
| `-y`, `--yes` | 跳過覆寫確認提示 |
| `-v`, `--verbose` | 顯示詳細操作資訊 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正規表達式的路徑 |
| `-t`, `--tui` | 使用純文字進度顯示 |
| `-n`, `--dry-run` | 預覽但不執行 |
| `-V`, `--verify` | 移動後校驗檔案完整性 |
| `-C`, `--resume` | 斷點續傳（僅跨裝置回退時） |
| `-s`, `--strict` | 嚴格雜湊校驗續傳 |
| `-a`, `--append` | 跨裝置移動的追加模式 |
| `--sync` | 同步到磁碟（僅跨裝置） |

**範例：**

```bash
# 移動單個檔案
bcmr move old_file.txt new_location/

# 遞迴移動目錄
bcmr move -r old_project/ new_location/

# 帶排除的移動
bcmr move -r --exclude='^node_modules' --exclude='\.log$' project/ dest/

# 乾跑
bcmr move -r -n old_project/ new_location/
```

::: tip
同裝置移動使用 `rename(2)` 系統呼叫，瞬間完成。跨裝置移動自動回退到複製+刪除，並帶進度追蹤。
:::

---

## remove

刪除檔案和目錄。

```
bcmr remove [選項] <路徑>...
```

| 參數 | 說明 |
|------|------|
| `-r`, `--recursive` | 遞迴刪除目錄 |
| `-f`, `--force` | 強制刪除，不確認 |
| `-y`, `--yes` | 跳過確認提示 |
| `-i`, `--interactive` | 逐個確認刪除 |
| `-v`, `--verbose` | 顯示詳細操作資訊 |
| `-d`, `--dir` | 僅刪除空目錄（類似 `rmdir`） |
| `-e`, `--exclude <PATTERN>` | 排除匹配正規表達式的路徑 |
| `-t`, `--tui` | 使用純文字進度顯示 |
| `-n`, `--dry-run` | 預覽但不執行 |

**範例：**

```bash
# 刪除單個檔案
bcmr remove unnecessary.txt

# 刪除多個檔案（萬用字元）
bcmr remove *.log

# 遞迴刪除目錄
bcmr remove -r old_project/

# 互動式刪除
bcmr remove -i file1.txt file2.txt file3.txt

# 帶排除的刪除
bcmr remove -r --exclude='\.important$' --exclude='\.backup$' trash/

# 乾跑
bcmr remove -r -n potentially_important_folder/
```

---

## init

產生 shell 整合指令碼。詳見 [Shell 整合](/zh-Hant/guide/shell-integration)。

```
bcmr init <SHELL> [選項]
```

| 參數 | 說明 |
|------|------|
| `<SHELL>` | `bash`、`zsh` 或 `fish` |
| `--cmd <前綴>` | 命令前綴（如 `b` → `bcp`、`bmv`、`brm`） |
| `--prefix <前綴>` | 顯式前綴（覆寫 `--cmd`） |
| `--suffix <後綴>` | 命令後綴 |
| `--no-cmd` | 不建立別名 |
| `--path <路徑>` | 新增目錄到 PATH |

**範例：**

```bash
eval "$(bcmr init zsh --cmd b)"          # bcp, bmv, brm
eval "$(bcmr init bash --cmd '')"         # cp, mv, rm
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"  # pcp+, pmv+, prm+
```

---

## completions

產生 shell 補全指令碼。設定方法詳見 [Shell 整合](/zh-Hant/guide/shell-integration#shell-補全)。

```
bcmr completions <SHELL>
```

支援的 shell：`bash`、`zsh`、`fish`、`powershell`、`elvish`。

**範例：**

```bash
bcmr completions zsh > ~/.zfunc/_bcmr
bcmr completions bash > /etc/bash_completion.d/bcmr
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
bcmr completions powershell >> $PROFILE
```

---

## update

檢查更新並從 GitHub Releases 自我更新二進位檔案。

```
bcmr update
```

下載當前平台的最新版本並原地替換二進位檔案。

BCMR 也會在每次命令執行時在背景檢查更新（可透過 [設定](/zh-Hant/guide/configuration) 中的 `update_check` 控制）。
