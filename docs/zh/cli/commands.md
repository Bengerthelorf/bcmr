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
