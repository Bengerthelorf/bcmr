---
title: SSH 连接选项与 URI 路径
section: guide
order: 6
locale: zh
---

临时 SSH 连接选项可以放在子命令前后：

```bash
bcmr --ssh-port 2222 copy report.pdf alice@storage:/srv/backups/
bcmr copy --ssh-identity ~/.ssh/work_ed25519 --ssh-jump bastion report.pdf storage:backup/
bcmr --ssh-config ./ssh.conf doctor storage
```

| 选项 | 本次调用中的作用 |
|---|---|
| `--ssh-port PORT` | 目标端口，范围为 1–65535。 |
| `--ssh-identity PATH` | 提供指定身份文件，可重复传入多份。 |
| `--ssh-config PATH` | 读取指定 OpenSSH 配置文件。 |
| `--ssh-jump HOST` | 使用 OpenSSH ProxyJump，支持跳板链和 `user@host:port`。 |

这些选项覆盖源路径展开、传输、校验、远程文件检查、移动清理、`doctor`
和 `deploy`，也用于 serve/direct 建连及 SFTP 后端。它们不会修改持久配置；
SSH 别名与其余认证行为沿用 OpenSSH 配置。

`--ssh-identity` 追加身份参数，不会自动设置 `IdentitiesOnly`，因此仍可能
提供配置中的其他密钥或 agent 身份。需要限制认证身份时，在 SSH 配置中设置
`IdentitiesOnly yes` 和所需的 `IdentityFile`；需要禁用 agent 时再设置
`IdentityAgent none`。

`-P` 仍表示传输并发数；`--config` 选择 **bcmr 配置**，`--ssh-config`
选择 **OpenSSH 配置**。例如：

```bash
bcmr --config ./bcmr.toml --ssh-config ./ssh.conf copy -P 4 report.pdf storage:backup/
```

## URI 端点与端口优先级

URI 格式为 `scp://[user@]host[:port]/absolute/path`，路径始终为绝对路径。
需要相对于远程登录目录的路径时，使用 `storage:backup/`。
原有 `storage:/srv/backups/` 和 `storage://srv/backups/` 写法继续可用。

```bash
# 此端点使用 2200，覆盖本次调用设置的 2222。
bcmr --ssh-port 2222 copy report.pdf 'scp://alice@storage:2200/srv/backups/'

# IPv6 地址使用方括号；在 shell 中为 URI 加引号。
bcmr copy 'scp://alice@[2001:db8::1]:2222/srv/data/report%20Q4.csv' ./downloads/

# 按 UTF-8 解码百分号转义：空格、字面百分号和 Unicode 文件名。
bcmr copy 'scp://storage/srv/data/%E9%9B%AA%20%23100%25.txt' ./downloads/
```

URI 内的显式端口只对该端点优先于 `--ssh-port`。没有显式端口的端点先使用
`--ssh-port`，再使用 OpenSSH 的常规端口配置。同一主机上的不同端口在连接池
和连接复用中保持独立。

空格编码为 `%20`，`%` 为 `%25`，`#` 为 `%23`，`?` 为 `%3F`。
百分号转义必须能解码为合法 UTF-8。NUL、格式错误的主机部分、无效端口，
以及未转义的查询或片段分隔符，会在远程源路径展开前被拒绝。
URI 写法不改变传输后端或远程路径安全规则；解码后的源路径仍按 `*` 规则展开。

## 后台任务与补全

后台工作进程会重放原命令中的连接选项：

```bash
bcmr --ssh-port 2222 copy --background --json report.pdf storage:backup/
```

生成的 Bash、Zsh、Fish 和 PowerShell 远程补全会把已输入的连接选项转交给
远程补全请求，包括重复的身份文件选项。升级后请重新执行 `bcmr init`
初始化，或重新生成已安装的补全脚本。详见 [Shell 集成](./shell-integration.md)。
