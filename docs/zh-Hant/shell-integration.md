---
title: Shell 整合
section: guide
order: 2
locale: zh-Hant
---

BCMR 提供類似 zoxide 的 shell 整合。你可以建立帶自訂前綴、後綴的別名，或完全替換原生命令。

## 設定

在 shell 設定檔中加入以下內容：

:::code-group

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

## Shell 補全

BCMR 透過 `bcmr completions` 提供所有命令和參數的 tab 補全。

:::code-group

```bash [Zsh]
# 加入 ~/.zshrc
eval "$(bcmr completions zsh)"

# 或產生到 fpath（啟動更快）
bcmr completions zsh > ~/.zfunc/_bcmr
# 確保 ~/.zshrc 中有: fpath=(~/.zfunc $fpath)
```

```bash [Bash]
# 加入 ~/.bashrc
eval "$(bcmr completions bash)"

# 或產生到系統補全目錄
bcmr completions bash > /etc/bash_completion.d/bcmr
```

```fish [Fish]
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
```

```powershell [PowerShell]
# 如果 profile 目錄不存在則先建立，再追加
New-Item -Path (Split-Path $PROFILE) -ItemType Directory -Force | Out-Null
bcmr completions powershell >> $PROFILE

# 或僅在目前工作階段載入
bcmr completions powershell | Out-String | Invoke-Expression
```

:::

設定後即可 tab 補全命令和參數：

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```

:::callout[別名補全（Zsh）]{kind="info"}
使用 `bcmr init zsh --cmd <前綴>` 時，別名命令（如 `bcp`、`bmv`、`brm`）的補全會自動包含，無需額外設定。只需確保 `~/.zshrc` 中同時有：

```bash
eval "$(bcmr init zsh --cmd b)"
eval "$(bcmr completions zsh)"
```

之後即可直接對別名命令進行 tab 補全：

```
bcp -<TAB>   → --recursive --preserve --force --verify ...
bmv -<TAB>   → --recursive --preserve --force --verify ...
```
:::
