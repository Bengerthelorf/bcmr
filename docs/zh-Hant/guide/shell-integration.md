# Shell 整合

BCMR 提供類似 zoxide 的 shell 整合。你可以建立帶自訂前綴、後綴的別名，或完全替換原生命令。

## 設定

在 shell 設定檔中加入以下內容：

::: code-group

```bash [Zsh (~/.zshrc)]
# 使用 'b' 前綴 → bcp, bmv, brm
eval "$(bcmr init zsh --cmd b)"
```

```bash [Bash (~/.bashrc)]
# 使用 'b' 前綴 → bcp, bmv, brm
eval "$(bcmr init bash --cmd b)"
```

```fish [Fish (~/.config/fish/config.fish)]
# 使用 'b' 前綴 → bcp, bmv, brm
bcmr init fish --cmd b | source
```

:::

## 選項

| 參數 | 說明 |
|------|------|
| `--cmd <前綴>` | 設定命令前綴（如 `b` 建立 `bcp`、`bmv`、`brm`） |
| `--prefix <前綴>` | 顯式設定前綴（覆寫 `--cmd`） |
| `--suffix <後綴>` | 設定命令後綴 |
| `--no-cmd` | 不建立命令別名 |
| `--path <路徑>` | 新增目錄到 PATH |

## 範例

```bash
# 替換原生命令（建立 cp, mv, rm）
eval "$(bcmr init zsh --cmd '')"

# 自訂前綴（建立 testcp, testmv, testrm）
eval "$(bcmr init zsh --cmd test)"

# 前綴 + 後綴（建立 pcp+, pmv+, prm+）
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"
```

## 支援的 Shell

- Bash
- Zsh
- Fish
