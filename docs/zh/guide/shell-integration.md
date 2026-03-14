# Shell 集成

BCMR 提供类似 zoxide 的 shell 集成。你可以创建带自定义前缀、后缀的别名，或完全替换原生命令。

## 设置

在 shell 配置文件中添加以下内容：

::: code-group

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
