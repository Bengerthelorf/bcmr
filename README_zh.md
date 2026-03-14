<div align="center">

# BCMR

**更好的复制、移动、删除 — 现代化、安全的文件操作 CLI 工具，支持进度显示、断点续传和远程复制。**

[![Crates.io](https://img.shields.io/crates/v/bcmr?style=for-the-badge&color=blue)](https://crates.io/crates/bcmr)
&nbsp;
[![Documentation](https://img.shields.io/badge/文档-查看_→-2ea44f?style=for-the-badge)](https://bengerthelorf.github.io/bcmr/zh/)
&nbsp;
[![Homebrew](https://img.shields.io/badge/Homebrew-可用-orange?style=for-the-badge)](https://github.com/Bengerthelorf/bcmr#安装)

[English](README.md)

<br>

![Demo](img/demo.gif)

<br>

### [📖 阅读完整文档 →](https://bengerthelorf.github.io/bcmr/zh/)

安装、Shell 集成、CLI 参考、配置等。

</div>

---

## 亮点

- 📊 **进度显示** — 精美 TUI 界面，渐变进度条、ETA、速度、逐文件追踪。也提供纯文本模式
- 🔄 **断点续传与校验** — 通过 mtime、大小或 BLAKE3 哈希续传中断的传输。复制后可验证完整性
- 🌐 **远程复制 (SSH)** — 使用 SCP 风格语法通过 SSH 上传和下载
- ⚡ **默认高性能** — Reflink (写时复制)、Linux `copy_file_range`、稀疏文件检测、流水线扫描+复制
- 🛡️ **安全操作** — 干运行预览、覆盖提示、正则排除、原子写入
- 🎨 **可配置** — 通过 TOML 自定义颜色渐变、进度条字符、边框样式

## 安装

### Homebrew (macOS / Linux)

```bash
brew install Bengerthelorf/tap/bcmr
```

### 安装脚本

```bash
curl -fsSL https://bcmr.snaix.homes/ | bash
```

### Cargo

```bash
cargo install bcmr
```

### 预编译二进制

从 [Releases](https://github.com/Bengerthelorf/bcmr/releases/latest) 下载 — 支持 Linux (x86_64 musl 静态链接)、macOS Intel、macOS Apple Silicon 和 Windows。

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
```

### Shell 集成

```bash
# 添加到 ~/.zshrc 或 ~/.bashrc:
eval "$(bcmr init zsh --cmd b)"    # 创建 bcp, bmv, brm

# 或替换原生命令:
eval "$(bcmr init zsh --cmd '')"   # 创建 cp, mv, rm
```

> **需要帮助？** 查看 [快速开始](https://bengerthelorf.github.io/bcmr/zh/guide/getting-started) 指南，或浏览完整 [文档](https://bengerthelorf.github.io/bcmr/zh/)。

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
```

## 贡献

欢迎提交 Issue 和 PR！请访问 [GitHub Issues](https://github.com/Bengerthelorf/bcmr/issues)。

## 许可证

GPL-3.0 © [Zane Leong](https://github.com/Bengerthelorf)
