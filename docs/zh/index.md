---
layout: home
hero:
  name: BCMR
  text: 更好的复制、移动、删除
  tagline: 更现代的 cp / mv / scp — 每次复制自带 BLAKE3 完整性校验、崩溃后可续传、本地和 SSH 远程共用一个命令。
  image:
    src: /images/demo.gif
    alt: BCMR 演示
  actions:
    - theme: brand
      text: 快速开始
      link: /zh/guide/getting-started
    - theme: alt
      text: CLI 参考
      link: /zh/cli/
    - theme: alt
      text: GitHub
      link: https://github.com/Bengerthelorf/bcmr

features:
  - icon: ✅
    title: 默认完整性校验
    details: "每次复制都在 write 过程中流式跑 BLAKE3。--verify 升级为完整的 2-pass 校验 —— 不是 rsync --checksum 那样的可选重扫。"
  - icon: 🔄
    title: 崩溃安全续传
    details: "被打断后同一条命令再跑一次就能续上 —— session 文件、尾块校验、从停下的位置继续。不需要 --partial --append-verify 组合咒语。"
  - icon: 🔗
    title: 本地 SSH 同一 CLI
    details: "bcmr copy a.txt /b/ 和 bcmr copy a.txt user@host:/b/ 是同一条命令、同一套 flag。不需要在 cp/scp/rsync 之间切上下文。"
  - icon: 🔐
    title: Direct-TCP 快速通道
    details: "可选的 AES-256-GCM over 直连 TCP 数据面，绕开 SSH 单流加密瓶颈。密钥仍由 SSH 控制通道协商，身份认证不变。"
  - icon: ⚡
    title: 默认高性能
    details: Reflink（写时复制）、Linux copy_file_range、稀疏文件检测、扫描+复制流水线即时启动。
  - icon: 🛡️
    title: 默认安全
    details: "通过临时文件+重命名的原子写入、持久 fsync（macOS 用 F_FULLFSYNC）、干运行预览、正则排除。"
  - icon: 🤖
    title: 为人和 Agent 同时设计
    details: "TUI 实时进度与 ETA 供人阅读。--json 脱离终端转入后台，流式写 NDJSON；bcmr status <id> 返回状态机。不怕关终端。"
  - icon: 🗜️
    title: 线路压缩与去重
    details: "握手时协商每块 LZ4/Zstd（源码类文本 ~5×），外加 ≥ 16 MiB 重传的内容寻址去重。"
---
