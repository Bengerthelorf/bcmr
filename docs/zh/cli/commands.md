# 命令参考

---

## copy

复制文件和目录。

```
bcmr copy [选项] <源路径>... <目标路径>
```

| 参数 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归复制目录 |
| `-p`, `--preserve` | 保留文件属性（权限、时间戳） |
| `-f`, `--force` | 覆盖已有文件 |
| `-y`, `--yes` | 跳过覆盖确认提示 |
| `-v`, `--verbose` | 显示详细操作信息 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正则的路径 |
| `-t`, `--tui` | 使用纯文本进度显示 |
| `-n`, `--dry-run` | 预览但不执行 |
| `-V`, `--verify` | 复制后校验文件完整性 (BLAKE3) |
| `-C`, `--resume` | 断点续传（大小 + mtime 检查） |
| `-s`, `--strict` | 严格 BLAKE3 哈希校验续传 |
| `-a`, `--append` | 追加模式（仅检查大小，忽略 mtime） |
| `--sync` | 复制后同步到磁盘 (fsync) |
| `--reflink <MODE>` | 写时复制：`auto`（默认）、`force`、`disable` |
| `--sparse <MODE>` | 稀疏文件：`auto`（默认）、`force`、`disable` |
| `-P`, `--parallel <N>` | 远程复制的并行传输数（默认：配置值） |
| `-j`, `--jobs <N>` | 本地多文件并发复制数（默认：`min(CPU, 8)`） |
| `--compress <MODE>` | 远程线路压缩：`auto`（默认，同时通告 LZ4+Zstd）、`zstd`、`lz4`、`none` |
| `--fast` | 仅远程：跳过服务端 BLAKE3（`Ok { hash: None }`）；Linux 服务端额外启用 `splice(2)`。只有当你通过别的途径确认完整性时（例如 `-V` 在客户端重新哈希）才使用。 |

**示例：**

```bash
# 复制单个文件
bcmr copy document.txt backup/

# 复制多个文件（shell 通配符）
bcmr copy *.txt *.md backup/

# 递归复制目录
bcmr copy -r projects/ backup/

# 带属性保留的复制
bcmr copy -rp important_dir/ /backup/

# 强制覆盖且不提示
bcmr copy -fy source.txt destination.txt

# 排除模式（正则）
bcmr copy -r --exclude='\.git' --exclude='\.tmp$' src/ dest/

# 带校验的复制
bcmr copy --verify critical_data.db /backup/

# 断点续传
bcmr copy -C large_file.iso /backup/

# SSH 远程复制
bcmr copy local_file.txt user@host:/remote/path/

# 并行远程上传（4 worker）
bcmr copy -P 4 file1.bin file2.bin user@host:/remote/
```

### 续传模式

| 参数 | 行为 |
|------|------|
| `-C`（resume） | 比较 mtime — 匹配则从断点追加；不匹配则覆盖 |
| `-s`（strict） | 比较 BLAKE3 部分哈希 — 匹配则追加；不匹配则覆盖 |
| `-a`（append） | 目标较小则追加，大小相同则跳过，目标较大则覆盖 |

---

## move

移动文件和目录。

```
bcmr move [选项] <源路径>... <目标路径>
```

| 参数 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归移动目录 |
| `-p`, `--preserve` | 保留文件属性 |
| `-f`, `--force` | 覆盖已有文件 |
| `-y`, `--yes` | 跳过覆盖确认提示 |
| `-v`, `--verbose` | 显示详细操作信息 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正则的路径 |
| `-t`, `--tui` | 使用纯文本进度显示 |
| `-n`, `--dry-run` | 预览但不执行 |
| `-V`, `--verify` | 移动后校验文件完整性 |
| `-C`, `--resume` | 断点续传（仅跨设备回退时） |
| `-s`, `--strict` | 严格哈希校验续传 |
| `-a`, `--append` | 跨设备移动的追加模式 |
| `--sync` | 同步到磁盘（仅跨设备） |
| `-j`, `--jobs <N>` | 当 move 回退为 copy+delete 时的并发文件数（默认 `min(CPU, 8)`） |
| `--compress <MODE>` | 远程 move 的线路压缩：`auto`、`zstd`、`lz4`、`none` |
| `--fast` | 仅远程：跨设备 move 的复制阶段跳过服务端 BLAKE3 |

**示例：**

```bash
# 移动单个文件
bcmr move old_file.txt new_location/

# 递归移动目录
bcmr move -r old_project/ new_location/

# 带排除的移动
bcmr move -r --exclude='^node_modules' --exclude='\.log$' project/ dest/

# 干运行
bcmr move -r -n old_project/ new_location/
```

::: tip
同设备移动使用 `rename(2)` 系统调用，瞬间完成。跨设备移动自动回退到复制+删除，并带进度追踪。
:::

---

## remove

删除文件和目录。

```
bcmr remove [选项] <路径>...
```

| 参数 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归删除目录 |
| `-f`, `--force` | 强制删除，不确认 |
| `-y`, `--yes` | 跳过确认提示 |
| `-i`, `--interactive` | 逐个确认删除 |
| `-v`, `--verbose` | 显示详细操作信息 |
| `-d`, `--dir` | 仅删除空目录（类似 `rmdir`） |
| `-e`, `--exclude <PATTERN>` | 排除匹配正则的路径 |
| `-t`, `--tui` | 使用纯文本进度显示 |
| `-n`, `--dry-run` | 预览但不执行 |

**示例：**

```bash
# 删除单个文件
bcmr remove unnecessary.txt

# 删除多个文件（通配符）
bcmr remove *.log

# 递归删除目录
bcmr remove -r old_project/

# 交互式删除
bcmr remove -i file1.txt file2.txt file3.txt

# 带排除的删除
bcmr remove -r --exclude='\.important$' --exclude='\.backup$' trash/

# 干运行
bcmr remove -r -n potentially_important_folder/
```

---

## init

生成 shell 集成脚本。详见 [Shell 集成](/zh/guide/shell-integration)。

```
bcmr init <SHELL> [选项]
```

| 参数 | 说明 |
|------|------|
| `<SHELL>` | `bash`、`zsh` 或 `fish` |
| `--cmd <前缀>` | 命令前缀（如 `b` → `bcp`、`bmv`、`brm`） |
| `--prefix <前缀>` | 显式前缀（覆盖 `--cmd`） |
| `--suffix <后缀>` | 命令后缀 |
| `--no-cmd` | 不创建别名 |
| `--path <路径>` | 添加目录到 PATH |

**示例：**

```bash
eval "$(bcmr init zsh --cmd b)"          # bcp, bmv, brm
eval "$(bcmr init bash --cmd '')"         # cp, mv, rm
eval "$(bcmr init zsh --cmd --prefix p --suffix +)"  # pcp+, pmv+, prm+
```

---

## completions

生成 shell 补全脚本。设置方法详见 [Shell 集成](/zh/guide/shell-integration#shell-补全)。

```
bcmr completions <SHELL>
```

支持的 shell：`bash`、`zsh`、`fish`、`powershell`、`elvish`。

**示例：**

```bash
bcmr completions zsh > ~/.zfunc/_bcmr
bcmr completions bash > /etc/bash_completion.d/bcmr
bcmr completions fish > ~/.config/fish/completions/bcmr.fish
bcmr completions powershell >> $PROFILE
```

---

## update

检查更新并从 GitHub Releases 自更新二进制文件。

```
bcmr update
```

下载当前平台的最新版本并原地替换二进制文件。

BCMR 也会在每次命令运行时在后台检查更新（可通过 [配置](/zh/guide/configuration) 中的 `update_check` 控制）。

---

## deploy

将 bcmr 部署到远程主机以支持 [serve 协议](/zh/guide/remote-copy#serve-协议-加速传输)。

```
bcmr deploy <TARGET> [--path <PATH>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `<TARGET>` | 必需 | 远程目标（`user@host`） |
| `--path` | `~/.local/bin/bcmr` | 远程安装路径 |

自动检测远端 OS 和架构。相同平台直接传输本地二进制文件，跨平台从 GitHub Releases 下载。

```bash
bcmr deploy user@server
bcmr deploy root@10.0.0.1 --path /usr/local/bin/bcmr
```

---

## check

对比源和目标，不做任何修改。报告新增、修改和缺失的文件。

```
bcmr check [OPTIONS] <SOURCES>... <DESTINATION>
```

| 标志 | 说明 |
|------|------|
| `-r`, `--recursive` | 递归比较目录 |
| `-e`, `--exclude <PATTERN>` | 排除匹配正则的路径 |

**退出码：** `0` = 已同步，`1` = 存在差异，`2` = 错误。

比较基于文件大小和修改时间，不做内容哈希。

**示例：**

```bash
# 比较两个目录
bcmr check -r src/ backup/

# JSON 输出（适用于脚本 / AI Agent）
bcmr check --json -r src/ backup/
```

---

## status

查询 `--json` 后台任务的状态（v0.5.4+）。

```
bcmr status [JOB_ID] [--json]
```

当 `bcmr copy`、`move`、`remove` 携带 `--json` 运行时，主进程会脱离终端进入后台，进度以 NDJSON 写入 `~/.local/share/bcmr/jobs/<id>.jsonl`。`bcmr status` 读取该日志并把任务分类到五种状态之一：

| 状态 | 含义 |
|------|------|
| `scanning` | 进程存活，仍在遍历源或等待首次进度事件 |
| `running` | 进程存活，处于传输阶段 |
| `done` | 已结束，`status:"success"` |
| `failed` | 已结束，`status:"error"`（日志里保留了错误信息） |
| `interrupted` | 进程已不在了但日志里没有终止事件（崩溃 / 被 kill / OOM） |

**示例：**

```bash
# 列出最近的所有任务（tab 分隔：id、state、last %）
bcmr status

# 查单个任务（打印日志最新一行）
bcmr status 764dee358ff4

# 脚本友好的结构化输出：{ job_id, state, latest }
bcmr status 764dee358ff4 --json
```

新任务启动时会自动清理 7 天前的日志。

---

## 全局标志

以下标志适用于所有命令：

| 标志 | 说明 |
|------|------|
| `--json` | 输出 NDJSON 流式进度和结构化结果（适用于 AI Agent 和脚本） |
| `-h`, `--help` | 打印帮助 |
| `-V`, `--version` | 打印版本 |

### JSON 输出

`copy`、`move`、`remove` 携带 `--json` 时，bcmr 会**脱离终端进入后台**，并在 stdout 打印一行 descriptor：

```json
{"job_id":"764dee358ff4","pid":36897,"log":"/home/.../jobs/764dee358ff4.jsonl"}
```

父进程立即退出。进度、错误、最终结果都会追加到日志文件，通过 [`bcmr status <job_id>`](#status) 查询。`check` 的 `--json` 保持前台行为（没有长时间运行的操作需要轮询）。

**进度行**（传输期间约每 200ms 输出一行）：

```json
{"type":"progress","operation":"Copying","bytes_done":1048576,"bytes_total":10485760,"percent":10.0,"speed_bps":52428800,"eta_secs":2,"file":"large.bin","file_size":10485760,"file_progress":1048576,"scanning":false}
```

`"scanning":true` 变体在 plan 阶段（遍历源）期间作为心跳发出 — 消费者可以用它确认任务存活，即使还没有任何字节被传输。

**结果行**（成功或失败时发出）：

```json
{"type":"result","status":"success","operation":"Copying","bytes_total":10485760,"duration_secs":3.2,"avg_speed_bps":3276800}
```

```json
{"type":"result","status":"error","operation":"Removing","bytes_total":0,"duration_secs":0.001,"error":"Cannot remove '/tmp/x': Is a directory (use -r for recursive removal)"}
```

**Check 输出**（`bcmr check --json`）：

```json
{"command":"check","status":"success","in_sync":false,"added":[{"path":"new.txt","size":4096,"src_size":4096,"is_dir":false}],"modified":[],"missing":[],"summary":{"added":1,"modified":0,"missing":0,"total_bytes":4096}}
```

JSON 模式下交互提示自动确认（`--json` 隐含 `--yes`）。
