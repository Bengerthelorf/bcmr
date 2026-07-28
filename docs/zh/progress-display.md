---
title: 进度显示
section: guide
order: 4
locale: zh
---

BCMR 为所有文件操作提供五种明确的进度显示模式。可使用
`--progress=auto|tui|inline|plain|off`，或在配置中设置同名的 `progress.style`。

## Auto 模式（默认）

Auto 会根据输出环境选择最丰富且安全的显示方式：

- 有足够空间且支持颜色的 TTY 使用 TUI
- 设置 `NO_COLOR` 或终端较小时使用无色 inline
- 管道、重定向和 `TERM=dumb` 使用稳定的 plain 最终摘要

## TUI 模式

TUI 界面包含：

- 带颜色渐变的总进度条
- 传输速度和 ETA
- 当前文件名和逐文件进度条
- 项目计数（用于删除操作）
- 扫描指示器（流水线模式实时显示已发现的文件数）

支持 Ctrl+C（清理临时文件后退出）和 Ctrl+Z（Unix 上挂起/恢复）。

## Inline 模式

无颜色的 3 行实时显示，适合不希望使用完整 TUI 的终端：

```
Copying: [=========-----------] 45%
12.34 MiB / 27.00 MiB | 5.67 MiB/s | ETA: 00:03
File: largefile.zip [====----] 50%
```

## Plain 与 Off 模式

`plain` 只输出稳定的最终摘要，不使用光标控制，适合日志和管道。`off`
完全关闭进度；`-q` / `--quiet` 是方便的快捷方式。

## 流水线扫描

当不需要覆盖提示或干运行时，BCMR 使用流水线模式 — 在目录扫描进行的同时立即开始复制。进度显示会展示扫描动画，文件计数实时更新，扫描完成后切换到正常进度视图。

## 自定义

详见 [配置](/zh/guide/configuration) 了解颜色渐变、进度条字符和边框样式。
