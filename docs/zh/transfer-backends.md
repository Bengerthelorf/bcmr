---
title: 传输后端
section: guide
order: 12
---

正式版 0.7.0 之后的开发源码新增临时连接参数、聚合限速、混合来源、客户端中转和 SFTP-only 后端。

```bash
bcmr copy --ssh-port 2222 --ssh-identity ~/.ssh/temp file 'host:~/new/'
bcmr copy --bandwidth-limit 10MiB/s -P 4 'host:large' ./
bcmr copy local.txt 'A100:remote.txt' ./Downloads/
bcmr copy -r -V 'A100:results' '4090:~/archive/'
bcmr move --remote-backend=sftp -p 'restricted:report' ./Downloads/
bcmr copy 'scp://user@[::1]:2222/tmp/report%20one' ./
```

`--remote-backend=auto` 按完整连接身份检查一次远程 exec；exec 不可用且实际 SFTP 握手成功时自动切换。`ssh` 显式使用 serve/Shell；`sftp` 只使用子系统。认证或网络错误分别诊断。全局 SSH 参数在通配符展开前生效，并贯通补全、部署、诊断和后台任务。

限速按整个命令交付的未压缩应用数据计量。`10KiB` 为 10×1024 B/s，`10kB` 为 10×1000 B/s，正整数为 B/s，均可加 `/s`。协议/SSH 开销、属性查询、校验读取及已验证的续传前缀不计入该数据量。所有工作线程和分发目标共用预算；同文件系统移动重命名不传输数据。限速会停用自动 reflink/零拷贝/CAS 捷径，强制 reflink 与限速同时使用时报错。

混合来源、SFTP 和远程中转先验证全部计划，再写目标，使用有界缓冲，不在本地保存完整中间文件。目前组合后端按选中的叶子顺序传输；`-P` 继续控制普通 SSH/serve 通道的工作线程。复制失败或取消时保留移动源；多目标全部成功后才执行捕获的源清理。树操作失败时，已完成的目标文件可能保留。

SFTP 无需 Shell 工具即可遍历、处理 `~/` 和 `*`、读取属性、链接和哈希、传输内容。普通新文件通过协商的 `hardlink@openssh.com` 发布，避免 OpenSSH v3 rename 的竞态回退；链接直接原子创建。缺少该扩展的服务器不能满足 bcmr 普通文件的不覆盖发布保证，因此拒绝写入。扩展依据见 [OpenSSH 协议说明](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL)。

SFTP v3 无法读取/复制已有目标的 ACL/xattr，也不能按身份条件交换目录项，因此已有 SFTP 目标即使 `-f` 也不会被替换；请选择新路径，或在允许 exec 时使用 SSH 后端。本地/SSH 目标的已验证续传前缀可用于暂存结果；SFTP 替换仍受上述安全边界约束。已完全一致的 SFTP 文件可通过续传校验后跳过，不发生替换。移动在读取哈希前保存源属性。SFTP 时间精度为秒，`~其他用户` 需改用绝对路径。SFTP 移动使用规范父路径、内容哈希和协议提供的属性保护源；协议没有 inode/ctime，无法识别内容和属性完全相同的替换，也无法隔离最后检查后的写入。

组合 SSH/SFTP 上传在写入前拒绝 `--sync`，因为仅文件 fsync 不能证明目录项已持久化；Unix 本地目标保留严格 fsync。组合后端不提供 direct AES TCP 或 bcmr zstd/lz4 帧，显式请求会在传输前报错。可在选中的 SSH 配置里启用 OpenSSH Compression。普通 serve 的压缩和 direct 支持继续有效。

移动时，来源在目标中发生同名冲突即使加 `-f` 也会拒绝。对已交付的远程或组合叶子，清理先隔离来源，再确认记录的每个目标仍包含交付的文件或链接，之后才解除来源链接。目标被改写或与隔离后的来源互为别名时取消清理。中断监听覆盖复制和清理阶段；JSON 在来源清理完成后才报告移动成功。这些保护不能替代文件系统快照。

SFTP 暂存使用规范父路径、私有所有权标记和锚定负载校验；发现命名空间变化时保留恢复数据并报告位置。`-p` 的文件属性在受保护的暂存清理完成后，通过已验证且保持打开的句柄设置；新目标短暂以私有暂存权限可见。未指定 `-p` 时，SFTP 普通新文件保留 0600 权限。协议可见属性完全一致的替换，以及最终检查后的并发写入仍无法识别。组合流不接受强制 reflink 或 sparse-always 策略。

远程或组合 `move -p` 要求最终文件对所有者可读、最终目录对所有者可搜索，以便清理前验证已发布的目标。缺少文件 owner-read 或目录 owner-execute 的模式会在写入前拒绝；`copy -p` 仍支持这些受限权限。可移植 `-p` 不复制来源 ACL，因此靠 ACL 可读的来源，在目标上可能无法再验证，不能据此自动删除来源。
