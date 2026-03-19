# 远程复制 (SSH)

BCMR 支持使用 SCP 风格语法通过 SSH 复制文件到远程主机或从远程主机复制文件。

## 语法

```bash
# 上传：本地 → 远程
bcmr copy local_file.txt user@host:/remote/path/

# 下载：远程 → 本地
bcmr copy user@host:/remote/file.txt ./local/

# 递归上传
bcmr copy -r local_dir/ user@host:/remote/path/

# 递归下载
bcmr copy -r user@host:/remote/dir/ ./local/
```

## 并行传输

使用 `-P` 参数同时传输多个文件：

```bash
# 4 个并行上传
bcmr copy -P 4 file1.bin file2.bin file3.bin file4.bin user@host:/remote/

# 递归上传，8 个 worker
bcmr copy -r -P 8 ./large_dataset/ user@host:/data/

# 并行下载
bcmr copy -P 3 user@host:/data/a.bin user@host:/data/b.bin ./local/
```

默认并行数通过 `[scp] parallel_transfers` 配置（默认：4）。使用 `-P 1` 或小量传输时按顺序执行。

TUI 和纯文本模式均会显示每个 worker 的状态：

```
Uploading: [████████░░░░░░░░░░░░░░░░░░] 42% [3/4w]
150 MiB / 350 MiB | 45.5 MiB/s | ETA: 04:32
[1] large.iso 53% | [2] backup.tar 78% | [3] data.csv 12% | [4] idle
```

## 压缩

启用 SSH 压缩可在慢速链路上减少传输时间。在 `[scp]` 中配置：

| 值 | 行为 |
|----|------|
| `"auto"` | 按扩展名判断，可压缩字节 >30% 时启用压缩（默认） |
| `"force"` | 始终启用 SSH 压缩 |
| `"off"` | 不压缩 |

`auto` 模式下，已知压缩格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被视为不可压缩。当大部分数据已经是压缩格式时，跳过压缩以避免 CPU 开销。

## 工作原理

- 使用现有的 SSH 配置（`~/.ssh/config`、密钥等）
- 在开始传输前验证 SSH 连接
- 通过 ControlMaster 复用 SSH 连接
- 通过 SSH 流式传输数据并追踪进度
- 支持上传和下载两个方向

::: warning 限制
- 无法直接在两个远程主机之间复制 — 请使用本地作为中转
- 远程传输不支持排除规则和高级复制功能（reflink、sparse、resume）
:::

## 路径检测

BCMR 通过 `[user@]host:path` 格式检测远程路径。以下模式被识别为本地路径，不会触发远程模式：

- 绝对路径（`/path/to/file`）
- 相对路径（`./file`、`../file`）
- 主目录（`~/file`）
- Windows 盘符（`C:\file`）
