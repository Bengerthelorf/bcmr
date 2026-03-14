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

## 工作原理

- 使用现有的 SSH 配置（`~/.ssh/config`、密钥等）
- 在开始传输前验证 SSH 连接
- 通过 SSH 流式传输数据并追踪进度
- 支持上传和下载两个方向

::: warning 限制
- 无法直接在两个远程主机之间复制 — 请使用本地作为中转
- SSH 必须配置为非交互式（基于密钥的）认证（`BatchMode=yes`）
- 远程传输不支持排除规则和高级复制功能（reflink、sparse、resume）
:::

## 路径检测

BCMR 通过 `[user@]host:path` 格式检测远程路径。以下模式被识别为本地路径，不会触发远程模式：

- 绝对路径（`/path/to/file`）
- 相对路径（`./file`、`../file`）
- 主目录（`~/file`）
- Windows 盘符（`C:\file`）
