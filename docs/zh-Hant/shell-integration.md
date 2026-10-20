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

```powershell [PowerShell ($PROFILE)]
# 使用 'b' 前綴 → bcp, bmv, brm
bcmr init powershell --cmd b | Out-String | Invoke-Expression
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
- PowerShell

## Shell 補全

BCMR 透過 `bcmr completions` 提供所有命令和參數的 tab 補全。

:::code-group

```bash [Zsh]
# 加入 ~/.zshrc；需要時會自動初始化 zsh 補全系統。
eval "$(bcmr init zsh --cmd b)"

# 可選：安裝產生的補全檔案，供 fpath 載入。
bcmr completions zsh --install
# 只列印實際安裝路徑，不寫入檔案：
bcmr completions zsh --install --print
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

`bcmr init zsh --cmd b` 會建立 `bcp`、`bmv` 和 `brm`，僅在需要時初始化 `compinit`，並載入本機及遠端路徑補全。可重複執行；既有的 zsh 補全設定應放在此行之前。`--no-cmd` 保持僅設定 PATH 的行為，不初始化補全。

`--install` 只寫入檔案，不會啟用目前 shell 的補全，也不會修改設定檔。命令會列印實際安裝目錄，包括未設定 `XDG_DATA_HOME` 時的預設目錄。使用檔案載入時，請先將該目錄加入 `fpath`，再執行 `autoload -Uz compinit; compinit`。上面的獨立 `init` 設定已足夠啟用命令包裝及補全。

若 `compinit` 提示目錄不安全，請用 `compaudit` 檢查並修正目錄擁有權或權限後重試。初始化保留這些安全檢查；補全失敗時會提供診斷，命令包裝仍可使用。

設定後即可 tab 補全命令和參數：

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```

:::callout[別名補全（Zsh）]{kind="info"}
使用 `bcmr init zsh --cmd <前綴>` 時，一行即可啟用命令包裝及補全：

```bash
eval "$(bcmr init zsh --cmd b)"
```

之後即可直接對命令進行 tab 補全：

```
bcp -<TAB>   → --recursive --preserve --force --verify ...
bmv -<TAB>   → --recursive --preserve --force --verify ...
```
:::

遠端主機無法使用時，Tab 補全預設保持安靜。可直接執行隱藏的補全命令查看錯誤，或為 Tab 補全開啟診斷：

```bash
bcmr __complete-remote 'user@example.com:/path/'
export BCMR_COMPLETION_DEBUG=1
# 關閉診斷輸出：
unset BCMR_COMPLETION_DEBUG
```
