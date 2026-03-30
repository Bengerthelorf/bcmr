<div align="center">

<img src="img/icon.svg" width="128" height="128" alt="BCMR">

# BCMR

**更好的复制、移动、删除 — 现代化、安全的文件操作 CLI 工具，支持进度显示、断点续传和远程复制。**

[![Crates.io](https://img.shields.io/crates/v/bcmr?style=for-the-badge&color=blue)](https://crates.io/crates/bcmr)
&nbsp;
[![Documentation](https://img.shields.io/badge/文档-查看_→-2ea44f?style=for-the-badge)](https://app.snaix.homes/bcmr/zh/)
&nbsp;
[![Homebrew](https://img.shields.io/badge/Homebrew-可用-orange?style=for-the-badge)](https://github.com/Bengerthelorf/bcmr#安装)

[English](README.md)

<br>

![Demo](img/demo.gif)

<br>

### [📖 阅读完整文档 →](https://app.snaix.homes/bcmr/zh/)

安装、Shell 集成、CLI 参考、配置等。

</div>

---

## 亮点

- 📊 **进度显示** — 精美 TUI 界面，渐变进度条、ETA、速度、逐文件追踪。也提供纯文本模式
- 🔄 **断点续传与校验** — 基于会话文件的崩溃安全续传，O(1) 尾块验证。始终在线的 BLAKE3 内联哈希，2-pass 验证复制
- 🌐 **远程复制 (SSH)** — 通过 SSH 上传下载。双端安装 bcmr 时使用二进制 `bcmr serve` 协议加速传输，自动回退至传统 SCP
- ⚡ **默认高性能** — Reflink (写时复制)、Linux `copy_file_range`、稀疏文件检测、流水线扫描+复制、独立 SSH 连接的并行传输
- 🛡️ **安全操作** — 干运行预览、覆盖提示、正则排除、原子写入与持久 fsync (macOS 使用 `F_FULLFSYNC`)
- 🔄 **自更新** — `bcmr update` 原地更新；每次运行自动后台检查新版本
- 🎨 **可配置** — 通过 TOML 自定义颜色渐变、进度条字符、边框样式

## 安装

### Homebrew (macOS / Linux)

```bash
brew install Bengerthelorf/tap/bcmr
```

### 安装脚本

```bash
curl -fsSL https://app.snaix.homes/bcmr/install | bash
```

### Cargo

```bash
cargo install bcmr
```

### 预编译二进制

从 [Releases](https://github.com/Bengerthelorf/bcmr/releases/latest) 下载 — 支持 Linux (x86_64/ARM64)、macOS (Intel/Apple Silicon)、Windows (x86_64/ARM64) 和 FreeBSD。

### 从源码构建

```bash
git clone https://github.com/Bengerthelorf/bcmr.git
cd bcmr
cargo build --release
```

## 快速上手

```bash
# 复制文件
bcmr copy document.txt backup/
bcmr copy -r projects/ backup/

# 移动文件
bcmr move old_file.txt new_location/
bcmr move -r old_project/ new_location/

# 删除文件
bcmr remove -r old_project/
bcmr remove -i file1.txt file2.txt    # 交互式

# 干运行 — 预览但不执行
bcmr copy -r -n projects/ backup/

# 断点续传
bcmr copy -C large_file.iso /backup/

# SSH 远程复制
bcmr copy local.txt user@host:/remote/
bcmr copy user@host:/remote/file.txt ./

# 并行 SCP 传输（4 个工作线程）
bcmr copy -P 4 *.bin user@host:/backup/
bcmr copy -P 8 -r project/ user@host:/backup/
```

### Shell 集成

```bash
# 添加到 ~/.zshrc 或 ~/.bashrc:
eval "$(bcmr init zsh --cmd b)"    # 创建 bcp, bmv, brm

# 或替换原生命令:
eval "$(bcmr init zsh --cmd '')"   # 创建 cp, mv, rm
```

> **需要帮助？** 查看 [快速开始](https://app.snaix.homes/bcmr/zh/guide/getting-started) 指南，或浏览完整 [文档](https://app.snaix.homes/bcmr/zh/)。

## 配置

创建 `~/.config/bcmr/config.toml`：

```toml
[progress]
style = "fancy"

[progress.theme]
bar_gradient = ["#CABBE9", "#7E6EAC"]
bar_complete_char = "█"
bar_incomplete_char = "░"
border_color = "#9E8BCA"

[progress.layout]
box_style = "rounded"    # "rounded", "double", "heavy", "single"

[copy]
reflink = "auto"         # "auto" 或 "never"
sparse = "auto"          # "auto" 或 "never"

[scp]
parallel_transfers = 4   # 默认并行 SCP 工作线程数
compression = "auto"     # "auto"、"force" 或 "off"

update_check = "notify"  # "notify"、"quiet" 或 "off"
```

## 贡献

欢迎提交 Issue 和 PR！请访问 [GitHub Issues](https://github.com/Bengerthelorf/bcmr/issues)。

## 许可证

GPL-3.0 © [Zane Leong](https://github.com/Bengerthelorf)
