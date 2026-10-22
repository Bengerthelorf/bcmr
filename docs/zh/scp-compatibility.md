---
title: scp 兼容性
section: guide
order: 9
---

本文说明 bcmr 0.7.1 的能力，对照 [OpenSSH scp 手册](https://man.openbsd.org/scp)。

| 能力 | bcmr 行为 |
|---|---|
| 上传、下载、多来源、递归目录 | 已支持，本地和远程采用一致的目标路径规则。 |
| 来源通配符 | 本地和远程均支持 `*`；zsh 下远程模式应加引号。 |
| 临时 SSH 端口、密钥、配置、跳板机 | `--ssh-port`、可重复的 `--ssh-identity`、`--ssh-config`、`--ssh-jump`；也继承 SSH 配置。`-P` 仍是并发数，`--config` 仍是 bcmr 配置。 |
| URI、IPv6、端点端口 | `scp://user@host:2222/绝对路径`，支持方括号 IPv6 和 UTF-8 百分号编码；URI 端口优先于 `--ssh-port`。 |
| 聚合限速 | `--bandwidth-limit 10MiB/s`，全部工作线程、多目标共用上限。 |
| 本地与远程混合来源 | 已支持，同一个计划检查所有来源和覆盖目标。 |
| 远程到远程 | 通过本机有界流式中转，两端凭据都保留在本机。 |
| 仅支持 SFTP 的账号 | 自动识别的真实 SFTP v3 后端，也可指定 `--remote-backend=sftp`；无需远程 exec。文件发布需要 `hardlink@openssh.com` 扩展。 |
| 密码及密钥口令认证 | 交互式传统 SSH/SFTP 使用 OpenSSH 原生终端提示；非交互环境采用批处理认证。serve 批处理认证不可用时可回退传统 SSH。 |
| 属性和链接 | `-p`、递归跟随链接及环检测、`--no-deref`；移动保留链接对象。精度与能力边界取决于后端。 |
| 校验、续传、预演、多目标、JSON | bcmr 的额外工作流；不支持的组合明确报错。 |

传统 SSH 回退运行远程 Shell 工具，与 SFTP 后端和历史 SCP 协议分别实现。serve 协议不兼容引起的回退，与通配符独立。

默认不覆盖、覆盖确认，以及拒绝远程 `..` 和裸文件系统根目录，是有意保留的差异。普通 `-p` 不承诺复制硬链接拓扑或任意 ACL/xattr。

示例与边界见 [SSH 参数](./ssh-options.md)、[传输后端](./transfer-backends.md) 和 [本地/远程语义](./local-remote-semantics.md)。已经安装 0.7.0 的用户需要升级到 0.7.1，才能使用本文新增的能力。
