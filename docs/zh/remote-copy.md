---
title: 远程复制 (SSH)
section: guide
order: 5
locale: zh
---

BCMR 支持使用 SCP 风格语法通过 SSH 复制文件到远程主机或从远程主机复制文件。

## 语法

### 源路径中的星号

复制和移动支持本地及远程源路径中的 `*`。直接调用 bcmr 时请加引号，让 Shell 将模式原样传入：

```bash
bcmr copy 'mini_m2:/Users/snaix/Downloads/deecamp中期检查*' ~/Downloads/
bcmr copy 'host:~/reports/report*' ./downloads/
```

执行 `eval "$(bcmr init zsh --cmd b)"` 后，`bcp`、`bmv` 也支持不加引号的星号。生成的 `noglob` 别名只作用于这两个包装命令，不改变 zsh 的 `NOMATCH` 设置或其他命令。升级后需要重新初始化。

`*` 匹配单个路径层级内的零个或多个字符，不跨 `/`。隐藏文件需显式以点号开头；匹配到目录仍需 `-r`。无匹配时在复制前报错。已存在的精确路径优先按字面处理，包括名称本身含 `*` 的文件；目标路径始终按字面处理。仅 `*` 有模式含义，`?`、方括号和递归 `**` 不作为额外操作符。多目标操作共用一次展开的源列表。

其他与 scp 的差异见 [scp 功能对照](./scp-compatibility.md)。

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

默认并行数通过 `[scp] parallel_transfers` 配置（默认：4）。
Serve 模式的目录批次最多使用这么多条相互独立的 SSH 会话。
即使 `-P` 更大，单文件上传也只建立一条会话，因为生产路径不会
为了分片而牺牲可见目标文件的崩溃安全性。

TUI 和纯文本模式均会显示每个 worker 的状态：

```
Uploading: [████████░░░░░░░░░░░░░░░░░░] 42% [3/4w]
150 MiB / 350 MiB | 45.5 MiB/s | ETA: 04:32
[1] large.iso 53% | [2] backup.tar 78% | [3] data.csv 12% | [4] idle
```

## 压缩

BCMR 有**两套独立**的压缩层，请区分：

### SSH 层（传统 SCP 路径）

远端没有安装 bcmr，传输回退到传统 SSH 通道时，SSH 传输层可以用 zlib 压缩。在 `[scp]` 中配置：

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

当远端也安装了匹配版本的 bcmr 时，传输自动使用
**bcmr serve 协议** — 通过持久 SSH 会话或有界会话池承载的
二进制帧协议。它消除逐文件 SSH 进程开销，并支持服务端哈希计算。

当前线路协议版本是 v2，并且有意不接受 v1 的 PUT 语义。远端没有
bcmr 或协议版本不匹配时，bcmr 会显示回退警告并使用传统 SCP。

### 在远端安装 bcmr

```bash
# 部署 bcmr 到远程主机
bcmr deploy user@host

# 升级系统安装（交互式终端中可输入 sudo 密码）
bcmr deploy user@host --path /usr/local/bin/bcmr --sudo

# 对多台服务器应用相同选项，分别部署
bcmr deploy A100 4090 --path /usr/bin/bcmr --sudo
```

`bcmr deploy` 分别检测每台服务器的 OS 和架构。相同平台时直接传输本地可执行文件，不同平台时从与本地版本号一致的 GitHub Release 下载对应文件。

版本检查和安装后验证均针对选定路径（默认 `~/.local/bin/bcmr`），不会被 PATH 中另一个安装的版本影响。安装成功和跳过的结果都会显示安装路径/版本，以及 PATH 实际选中的路径/版本；如果被其他安装遮蔽，会给出提示。使用 `--path` 可以指定需要升级的那一份安装。

各服务器依次独立部署，某台失败后继续处理其他目标，最后汇总成功、跳过和失败结果；存在失败时返回非零退出码。重跑同一命令时，会跳过选定路径下已是本地版本的服务器。

`--sudo` 在标准输入和标准错误均连接终端时，通过原生 SSH 终端输入普通 sudo 密码。非交互环境和 `--json` 模式使用 `sudo -n`，缺少免密权限时明确报错。SSH 登录仍使用配置的密钥和批处理模式。

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

### 原子上传与覆盖策略

Serve PUT 默认不覆盖已存在的目标。只有显式使用
`--force --yes`，客户端才会在 v2 协议中携带覆盖决定：

```bash
# 远端已有 report.pdf 时失败，并保留旧文件
bcmr copy report.pdf user@host:/backup/

# 同步落盘后原子替换（要求协商 serve CAP_SYNC）
bcmr copy --force --yes --sync report.pdf user@host:/backup/

# 显式校验完成后发布
bcmr copy --force --yes --verify report.pdf user@host:/backup/
```

服务端先写入私有、句柄绑定的事务，校验长度和哈希后才发布。
中断的强制覆盖会保留旧的最终路径；流水线目录上传对每个文件都
保持同样的属性。

新的传统 SSH 上传同样先写私有暂存文件。带 `-p` 或 `-V` 的上传会在
私有暂存路径上完成属性和校验，再发布。辅助 SSH 命令无法提供可检查的
目录 fsync，因此传统上传、保留链接的上传，以及同时带 `-p` 或 `-V` 的
上传使用 `--sync` 时，都会在写入目标之前明确报错。普通 serve 上传要求
所有连接都协商 `CAP_SYNC`，缺失该能力时不会静默降级。Unix 同步下载会
同步本地发布操作。

Copy 默认跟随链接；`--no-deref` 保留链接对象，包括断链。Move 始终保留
链接对象。这条远程路径使用 SSH 辅助命令，不支持 `--direct direct`；
dry-run 仍可预览。排除规则、目录属性、移动清理和平台边界见
[本地与远程行为](./local-remote-semantics.md)。

生产版暂时禁用单文件跨连接分片 PUT，因为独立连接还不能安全加入
同一个服务端事务。`-P N` 仍会并行处理递归批次中的不同文件。

### 内容寻址去重（`CAP_DEDUP`）

serve 协议自带块级去重：≥ 16 MiB 的 PUT 会先交换每个 4 MiB 块的 BLAKE3 哈希，服务端只要那些不在本地内容寻址存储（CAS）里的块。CAS 路径由 [`BCMR_CAS_DIR` / `BCMR_CAS_CAP_MB`](/zh/guide/configuration#环境变量) 控制。

从 0.7.1 起，Linux/macOS serve 缓存使用私有权限，并按固定的根目录身份隔离；不同根目录不能复用彼此的块。所有 serve 根目录共享一个容量限额，本地 CAS 保持独立缓存。升级后的首次上传会重新填充这份隔离缓存。FreeBSD、Windows 等其他平台服务端会请求全部数据块，暂不缓存去重块。缓存目录不可用或不安全时，禁用这项加速，普通传输继续。

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
- **传统模式**：默认通过 ControlMaster 复用 SSH 连接；设置 `BCMR_SSH_NO_MULTIPLEX=1` 可关闭
- **Serve 会话池**：显式禁用 ControlMaster，使并行成员使用相互独立的 TCP/加密流
- 每 15 秒探测无流量的 SSH 会话，连续 20 次无响应后关闭；持续有流量的慢连接不会被误判
- 同步下载会先一次性解析最深的既存目录前缀；指向有效目录的 symlink home/存储前缀可用，断裂 symlink 会在任何含糊目标被创建前报错
- 通过 SSH 流式传输数据并追踪进度
- 支持上传和下载两个方向

:::callout[限制]{kind="warn"}
- 远程到远程通过本机流式中转，详见 [传输后端](./transfer-backends.md)。
- Serve 快路径的断点续传（`-C`）：单文件上传已原生支持；递归目录上传和所有下载在指定 `--resume/--strict/--append` 时自动回退到传统模式
- 在共享可写目标目录中，中断的上传可能留下空的私有 `.bcmr.receive.*` 恢复目录；BCMR 不会在可竞争的命名空间中执行不安全的按名称清理
:::

## 路径检测

BCMR 通过 `[user@]host:path` 格式检测远程路径。以下模式被识别为本地路径，不会触发远程模式：

- 绝对路径（`/path/to/file`）
- 相对路径（`./file`、`../file`）
- 主目录（`~/file`）
- Windows 盘符（`C:\file`）

目标路径规则和仍未统一的行为见 [本地与远程行为对照](./local-remote-semantics.md)。

混合来源、聚合限速与 SFTP-only 账号详见 [传输后端](./transfer-backends.md)。
