# CLI 概览

BCMR 提供文件操作、差异比较、shell 集成和自更新等命令。

## 命令

| 命令 | 说明 |
|------|------|
| [`copy`](/zh/cli/commands#copy) | 复制文件和目录 |
| [`move`](/zh/cli/commands#move) | 移动文件和目录 |
| [`remove`](/zh/cli/commands#remove) | 删除文件和目录 |
| [`check`](/zh/cli/commands#check) | 比较源与目标差异 |
| [`init`](/zh/cli/commands#init) | 生成 shell 集成脚本 |
| [`update`](/zh/cli/commands#update) | 检查更新并自更新 |
| [`completions`](/zh/cli/commands#completions) | 生成 shell 补全脚本 |
| [`deploy`](/zh/cli/commands#deploy) | 部署 bcmr 到远程主机 |

## 全局参数

| 参数 | 说明 |
|------|------|
| `--json` | NDJSON 流式输出，适用于 AI Agent 和脚本 |
| `-h`, `--help` | 打印帮助 |
| `-V`, `--version` | 打印版本 |

## 通用参数

以下参数在 `copy`、`move` 和 `remove` 中通用：

| 参数 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归操作目录 |
| `-f`, `--force` | 覆盖已有文件 / 强制删除 |
| `-y`, `--yes` | 跳过确认提示 |
| `-v`, `--verbose` | 显示详细操作信息 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正则的路径 |
| `-t`, `--tui` | 使用纯文本进度显示 |
| `-n`, `--dry-run` | 预览操作但不执行 |

## 干运行

所有修改文件的命令都接受 `-n` / `--dry-run`，以彩色方案显示操作计划：

```bash
bcmr copy -r -n projects/ backup/
bcmr move -n old_file.txt new_location/
bcmr remove -r -n old_project/
```

操作以颜色区分：<span style="color: green">ADD</span>、<span style="color: yellow">OVERWRITE</span>、<span style="color: blue">APPEND</span>、<span style="color: cyan">MOVE</span>、<span style="color: grey">SKIP</span>、<span style="color: red">REMOVE</span>。
