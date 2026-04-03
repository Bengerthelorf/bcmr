---
layout: home
hero:
  name: BCMR
  text: 更好的复制、移动、删除
  tagline: 现代化、安全的文件操作 CLI 工具 — 支持进度显示、断点续传、完整性校验、SSH 远程复制和 AI Agent JSON 输出。
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
  - icon: 📊
    title: 进度显示
    details: 精美的 TUI 界面，支持渐变进度条、传输速度、ETA 和逐文件进度追踪。也提供纯文本模式。
  - icon: 🔄
    title: 断点续传与校验
    details: 支持通过 mtime、文件大小或 BLAKE3 哈希校验续传中断的传输。复制后可验证文件完整性。
  - icon: 🌐
    title: 远程复制 (SSH)
    details: 通过 SSH 并行传输、智能压缩、逐 worker 进度显示。无需额外工具。
  - icon: ⚡
    title: 默认高性能
    details: Reflink (写时复制)、Linux copy_file_range、稀疏文件检测、流水线扫描+复制即时启动。
  - icon: 🛡️
    title: 安全操作
    details: 干运行预览、覆盖提示、正则排除、通过临时文件+重命名实现原子写入。
  - icon: 🤖
    title: AI Agent 友好
    details: "--json 输出 NDJSON 流式进度和结构化结果。check 命令对比源与目标差异。专为程序化使用设计。"
  - icon: 🎨
    title: 完全可配置
    details: 自定义颜色渐变、进度条字符、边框样式，以及 reflink/sparse 默认值，均通过 TOML 配置。
---
