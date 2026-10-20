---
title: Shell 集成
section: guide
order: 2
locale: zh
---

BCMR 提供类似 zoxide 的 shell 集成。你可以创建带自定义前缀、后缀的别名，或完全替换原生命令。

## 设置

在 shell 配置文件中添加以下内容：

:::code-group

```bash [Zsh (~/.zshrc)]
# 使用 'b' 前缀 → bcp, bmv, brm
eval "$(bcmr init zsh --cmd b)"
```

```bash [Bash (~/.bashrc)]
# 使用 'b' 前缀 → bcp, bmv, brm
eval "$(bcmr init bash --cmd b)"
```

```fish [Fish (~/.config/fish/config.fish)]
# 使用 'b' 前缀 → bcp, bmv, brm
bcmr init fish --cmd b | source
```

```powershell [PowerShell ($PROFILE)]
# 使用 'b' 前缀 → bcp, bmv, brm
bcmr init powershell --cmd b | Out-String | Invoke-Expression
```

:::

## 选项

| 参数 | 说明 |
|------|------|
| `--cmd <前缀>` | 设置命令前缀（如 `b` 创建 `bcp`、`bmv`、`brm`） |
| `--prefix <前缀>` | 显式设置前缀（覆盖 `--cmd`） |
| `--suffix <后缀>` | 设置命令后缀 |
| `--no-cmd` | 不创建命令别名 |
| `--path <路径>` | 添加目录到 PATH |

## 示例

```bash
# 替换原生命令（创建 cp, mv, rm）
eval "$(bcmr init zsh --cmd '')"

# 自定义前缀（创建 testcp, testmv, testrm）
eval "$(bcmr init zsh --cmd test)"

# 前缀 + 后缀（创建 pcp+, pmv+, prm+）
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"
```

## 支持的 Shell

- Bash
- Zsh
- Fish
- PowerShell

## Shell 补全

BCMR 通过 `bcmr completions` 提供所有命令和参数的 tab 补全。

:::code-group

```bash [Zsh]
# 添加到 ~/.zshrc；需要时会自动初始化 zsh 补全系统。
eval "$(bcmr init zsh --cmd b)"

# 可选：安装生成的补全文件，供 fpath 加载。
bcmr completions zsh --install
# 只打印实际安装路径，不写入文件：
bcmr completions zsh --install --print
```

```bash [Bash]
# 添加到 ~/.bashrc
eval "$(bcmr completions bash)"

# 或生成到系统补全目录
bcmr completions bash > /etc/bash_completion.d/bcmr
```

```fish [Fish]
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
```

```powershell [PowerShell]
# 如果 profile 目录不存在则先创建，再追加
New-Item -Path (Split-Path $PROFILE) -ItemType Directory -Force | Out-Null
bcmr completions powershell >> $PROFILE

# 或仅在当前会话加载
bcmr completions powershell | Out-String | Invoke-Expression
```

:::

`bcmr init zsh --cmd b` 会创建 `bcp`、`bmv` 和 `brm`，仅在需要时初始化 `compinit`，并加载本地及远程路径补全。可重复执行；已有的 zsh 补全设置应放在此行之前。`--no-cmd` 保持仅配置 PATH 的行为，不初始化补全。

`--install` 只写入文件，不会激活当前 shell 的补全，也不会修改配置文件。命令会打印实际安装目录，包括未设置 `XDG_DATA_HOME` 时的默认目录。使用文件加载时，请先将该目录加入 `fpath`，再执行 `autoload -Uz compinit; compinit`。上面的独立 `init` 配置已足够启用命令包装及补全。

若 `compinit` 提示目录不安全，请用 `compaudit` 检查并修正目录所有权或权限后重试。初始化保留这些安全检查；补全失败时会给出诊断，命令包装仍可使用。

设置后即可 tab 补全命令和参数：

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```

:::callout[别名补全（Zsh）]{kind="info"}
使用 `bcmr init zsh --cmd <前缀>` 时，一行即可启用命令包装及补全：

```bash
eval "$(bcmr init zsh --cmd b)"
```

之后即可直接对命令进行 tab 补全：

```
bcp -<TAB>   → --recursive --preserve --force --verify ...
bmv -<TAB>   → --recursive --preserve --force --verify ...
```
:::

远程主机不可用时，Tab 补全默认保持安静。可直接执行隐藏的补全命令查看错误，或为 Tab 补全开启诊断：

```bash
bcmr __complete-remote 'user@example.com:/path/'
export BCMR_COMPLETION_DEBUG=1
# 关闭诊断输出：
unset BCMR_COMPLETION_DEBUG
```
