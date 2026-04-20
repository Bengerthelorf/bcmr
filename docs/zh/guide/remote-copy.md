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

BCMR 有**两套独立**的压缩层，请区分：

### SSH 层（传统 SCP 路径）

远端没有安装 bcmr，传输回退到 SCP 时，SSH 传输层可以用 zlib 压缩。在 `[scp]` 中配置：

| 值 | 行为 |
|----|------|
| `"auto"` | 按扩展名判断，可压缩字节 >30% 时启用压缩（默认） |
| `"force"` | 始终启用 SSH 压缩 |
| `"off"` | 不压缩 |

`auto` 模式下，已知压缩格式（`.gz`、`.zip`、`.mp4`、`.jpg` 等）被视为不可压缩。当大部分数据已经是压缩格式时，跳过压缩以避免 CPU 开销。

### 线路层（serve 协议，`--compress`）

双方都能说 [serve 协议](#serve-协议加速传输) 时，握手阶段协商出每块用 LZ4 还是 Zstd 压缩。这是**独立于并快于** SSH 自带的 zlib — Zstd-3 在源码风格文本上达到 ~320 MB/s 编码、~5× 缩减，zlib 的吞吐只有它的约 1/10。

| `--compress` 模式 | 客户端通告的 caps | 协商结果 |
|---|---|---|
| `auto`（默认） | LZ4 + Zstd | 双方都有 → Zstd-3；否则 LZ4；都没有 → 原始 |
| `zstd` | 仅 Zstd | 服务端也有 → Zstd-3；否则原始 |
| `lz4` | 仅 LZ4 | 服务端也有 → LZ4；否则原始 |
| `none`/`off` | 无 | 只发原始 `Data` 帧 |

每个 4 MiB 块会自动判断是否跳过压缩 — 编码后体积超过原始 95% 就直接发原始数据，这样已经压缩过的文件（`.jpg`、`.zst`、`/dev/urandom`）开启压缩几乎没有代价。真实链路上的压缩比和吞吐见 [Wire Protocol 消融实验](/ablation/wire-protocol#experiment-12-wire-compression-across-real-hosts)。

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

### 内容寻址去重（`CAP_DEDUP`）

serve 协议自带块级去重：≥ 16 MiB 的 PUT 会先交换每个 4 MiB 块的 BLAKE3 哈希，服务端只要那些不在本地内容寻址存储（CAS）里的块。CAS 路径由 [`BCMR_CAS_DIR` / `BCMR_CAS_CAP_MB`](/zh/guide/configuration#环境变量) 控制。

对每个够大的文件 PUT 自动生效 — 不需要额外 flag 启用。收益在“同一个构建产物反复上传”（开发循环）这种场景最明显：第二次上传跳过服务端已经有的所有块。

```bash
# 第一次：整个 64 MiB 都走线路
bcmr copy build/artifact.bin user@host:/deploy/

# 第二次：每个块都命中 CAS，实际线路字节数接近 0
bcmr copy build/artifact.bin user@host:/deploy/alt-name.bin
```

协议时序见 [去重实验](/ablation/wire-protocol#experiment-11-content-addressed-dedup-for-repeat-put)。

### 快速模式（`--fast`）

用"服务端省 CPU"换取跳过服务端 BLAKE3：

```bash
# 服务端 Ok 响应里 hash 为 None。
bcmr copy --fast user@host:/big.bin ./local.bin

# 跟 -V 组合：客户端重新 hash 目标文件
bcmr copy --fast -V user@host:/big.bin ./local.bin
```

服务端是 Linux 且 `--compress=none` 同时生效时，`--fast` 额外启用 `splice(2)` 做 file → stdout 零拷贝。splice 实现 [目前并不是所有场景都更快](/ablation/wire-protocol#experiment-14-cap-fast-real-numbers)；`--fast` 诚实记录了这一权衡，详情看 Internals。

默认关闭 — `--fast` 是显式放弃服务端完整性校验。

## 工作原理

- 使用现有的 SSH 配置（`~/.ssh/config`、密钥等）
- 在开始传输前验证 SSH 连接
- **Serve 模式**：通过 SSH 启动远端 `bcmr serve`，通过 stdin/stdout 的二进制协议通信
- **传统模式**：通过 ControlMaster 复用 SSH 连接，并行 worker 使用独立 TCP 连接
- 通过 SSH 流式传输数据并追踪进度
- 支持上传和下载两个方向

::: warning 限制
- 无法直接在两个远程主机之间复制 — 请使用本地作为中转
- Serve 快路径的断点续传（`-C`）：单文件上传已原生支持；递归目录上传和所有下载在指定 `--resume/--strict/--append` 时自动回退到传统模式
:::

## 路径检测

BCMR 通过 `[user@]host:path` 格式检测远程路径。以下模式被识别为本地路径，不会触发远程模式：

- 绝对路径（`/path/to/file`）
- 相对路径（`./file`、`../file`）
- 主目录（`~/file`）
- Windows 盘符（`C:\file`）
