# 快速开始

BCMR (Better Copy Move Remove) 是一个用 Rust 编写的现代化文件操作 CLI 工具，提供进度追踪、断点续传、完整性校验和 SSH 远程复制功能。

## 系统要求

- macOS (Intel 或 Apple Silicon)、Linux (x86_64) 或 Windows (x86_64)

## 安装

::: code-group

```bash [Homebrew]
brew install Bengerthelorf/tap/bcmr
```

```bash [安装脚本]
curl -fsSL https://bcmr.snaix.homes/ | bash
```

```bash [Cargo]
cargo install bcmr
```

```bash [从源码构建]
git clone https://github.com/Bengerthelorf/bcmr.git
cd bcmr
cargo build --release
# 二进制文件位于: ./target/release/bcmr
```

:::

预编译二进制文件（包括 Linux musl 静态链接版本）可在 [Releases 页面](https://github.com/Bengerthelorf/bcmr/releases/latest) 下载。

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

::: tip Shell 集成
可设置 shell 别名，让 `cp`、`mv`、`rm`（或自定义前缀）自动使用 BCMR。详见 [Shell 集成](/zh/guide/shell-integration)。
:::

## 下一步

- [Shell 集成](/zh/guide/shell-integration) — 替换或别名原生命令
- [配置](/zh/guide/configuration) — 自定义颜色、进度样式和复制行为
- [CLI 参考](/zh/cli/commands) — copy、move 和 remove 的完整命令参考
