---
title: 快速开始
section: guide
order: 1
locale: zh
---

BCMR (Better Copy Move Remove) 是一个用 Rust 编写的现代化文件操作 CLI
工具，提供进度追踪、断点续传、完整性校验和 SSH 远程复制功能。

如果还没装，先看 [**安装**](/install) —— 本页假定 `bcmr` 已经在 `$PATH`
上了。

## 快速上手

```bash
# 复制文件
bcmr copy document.txt backup/

# 递归复制目录
bcmr copy -r projects/ backup/

# 移动文件
bcmr move old_file.txt new_location/

# 确认后删除
bcmr remove -r old_project/

# 干运行 — 预览操作但不执行
bcmr copy -r -n projects/ backup/
```

:::callout[Shell 集成]{kind="info"}
可设置 shell 别名，让 `cp`、`mv`、`rm`（或自定义前缀）自动使用
bcmr。详见 [Shell 集成](/zh/guide/shell-integration)。
:::

## 下一步

- [Shell 集成](/zh/guide/shell-integration) — 替换或别名原生命令
- [配置](/zh/guide/configuration) — 颜色、进度样式、复制行为
- [远程复制](/zh/guide/remote-copy) — SSH 与 direct-tcp 快速通道
- [CLI 参考](/commands) — 所有子命令 / 标志，可搜索
