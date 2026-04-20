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

## Shell 补全

BCMR 通过 `bcmr completions` 提供所有命令和参数的 tab 补全。

:::code-group

```bash [Zsh]
# 添加到 ~/.zshrc
eval "$(bcmr completions zsh)"

# 或生成到 fpath（启动更快）
bcmr completions zsh > ~/.zfunc/_bcmr
# 确保 ~/.zshrc 中有: fpath=(~/.zfunc $fpath)
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

设置后即可 tab 补全命令和参数：

```
bcmr co<TAB>       → bcmr copy
bcmr copy -<TAB>   → --recursive --preserve --force --verify ...
```

:::callout[别名补全（Zsh）]{kind="info"}
使用 `bcmr init zsh --cmd <前缀>` 时，别名命令（如 `bcp`、`bmv`、`brm`）的补全会自动包含，无需额外配置。只需确保 `~/.zshrc` 中同时有：

```bash
eval "$(bcmr init zsh --cmd b)"
eval "$(bcmr completions zsh)"
```

之后即可直接对别名命令进行 tab 补全：

```
bcp -<TAB>   → --recursive --preserve --force --verify ...
bmv -<TAB>   → --recursive --preserve --force --verify ...
```
:::
