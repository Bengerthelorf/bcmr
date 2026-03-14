# CLI 概覽

BCMR 提供三個主要的檔案操作命令和一個 shell 整合初始化器。

## 命令

| 命令 | 說明 |
|------|------|
| [`copy`](/zh-Hant/cli/commands#copy) | 複製檔案和目錄 |
| [`move`](/zh-Hant/cli/commands#move) | 移動檔案和目錄 |
| [`remove`](/zh-Hant/cli/commands#remove) | 刪除檔案和目錄 |
| [`init`](/zh-Hant/cli/commands#init) | 產生 shell 整合指令碼 |

## 通用參數

以下參數在 `copy`、`move` 和 `remove` 中通用：

| 參數 | 說明 |
|------|------|
| `-r`, `--recursive` | 遞迴操作目錄 |
| `-f`, `--force` | 覆寫已有檔案 / 強制刪除 |
| `-y`, `--yes` | 跳過確認提示 |
| `-v`, `--verbose` | 顯示詳細操作資訊 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正規表達式的路徑 |
| `-t`, `--tui` | 使用純文字進度顯示 |
| `-n`, `--dry-run` | 預覽操作但不執行 |

## 乾跑

所有修改檔案的命令都接受 `-n` / `--dry-run`，以彩色方案顯示操作計畫：

```bash
bcmr copy -r -n projects/ backup/
bcmr move -n old_file.txt new_location/
bcmr remove -r -n old_project/
```

操作以顏色區分：<span style="color: green">ADD</span>、<span style="color: yellow">OVERWRITE</span>、<span style="color: blue">APPEND</span>、<span style="color: cyan">MOVE</span>、<span style="color: grey">SKIP</span>、<span style="color: red">REMOVE</span>。
