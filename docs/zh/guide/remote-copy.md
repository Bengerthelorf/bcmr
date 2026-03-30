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

## Serve 协议（加速传输）

当远端也安装了 bcmr 时，传输自动使用 **bcmr serve 协议** — 通过单个 SSH 连接的二进制帧协议。消除逐文件 SSH 进程开销，并支持服务端哈希计算。

远端没有 bcmr 时自动回退到传统 SCP。

### 在远端安装 bcmr

```bash
# 部署 bcmr 到远程主机
bcmr deploy user@host

# 自定义安装路径
bcmr deploy user@host --path /usr/local/bin/bcmr
```

`bcmr deploy` 自动检测远端 OS 和架构。相同平台时直接传输本地二进制文件，不同平台时从 GitHub Releases 下载对应版本。

### Serve 协议优势

| | 传统 SSH | Serve 协议 |
|---|---|---|
| 连接建立 | 每个文件一个进程 | 单个持久连接 |
| 文件列表 | `ssh find`（shell 解析） | 二进制 LIST 消息 |
| 哈希校验 | 需传回数据到本地 | 服务端直接计算 BLAKE3 |
| 上传校验 | 需重新下载校验 | 服务端返回写入后的哈希 |
| 单文件开销 | ~50ms（进程启动） | ~0.1ms（消息帧） |

### 校验远程传输

```bash
# 上传并校验完整性
bcmr copy -V local_file.txt user@host:/backup/

# 使用 serve 协议时，服务端在写入后计算哈希并返回
# 无需重新传输数据即可完成校验
```

## 工作原理

- 使用现有的 SSH 配置（`~/.ssh/config`、密钥等）
- 在开始传输前验证 SSH 连接
- **Serve 模式**：通过 SSH 启动远端 `bcmr serve`，通过 stdin/stdout 的二进制协议通信
- **传统模式**：通过 ControlMaster 复用 SSH 连接，并行 worker 使用独立 TCP 连接
- 通过 SSH 流式传输数据并追踪进度
- 支持上传和下载两个方向

::: warning 限制
- 无法直接在两个远程主机之间复制 — 请使用本地作为中转
- Serve 协议暂不支持断点续传（`-C`），需续传时使用传统模式（`-P 1`）
:::

## 路径检测

BCMR 通过 `[user@]host:path` 格式检测远程路径。以下模式被识别为本地路径，不会触发远程模式：

- 绝对路径（`/path/to/file`）
- 相对路径（`./file`、`../file`）
- 主目录（`~/file`）
- Windows 盘符（`C:\file`）
