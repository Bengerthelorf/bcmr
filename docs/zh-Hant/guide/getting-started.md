# 快速開始

BCMR (Better Copy Move Remove) 是一個用 Rust 編寫的現代化檔案操作 CLI 工具，提供進度追蹤、斷點續傳、完整性校驗和 SSH 遠端複製功能。

## 系統需求

- macOS (Intel 或 Apple Silicon)、Linux (x86_64) 或 Windows (x86_64)

## 安裝

::: code-group

```bash [Homebrew]
brew install Bengerthelorf/tap/bcmr
```

```bash [安裝指令碼]
curl -fsSL https://bcmr.snaix.homes/ | bash
```

```bash [Cargo]
cargo install bcmr
```

```bash [從原始碼建置]
git clone https://github.com/Bengerthelorf/bcmr.git
cd bcmr
cargo build --release
# 二進位檔案位於: ./target/release/bcmr
```

:::

預編譯二進位檔案（包括 Linux musl 靜態連結版本）可在 [Releases 頁面](https://github.com/Bengerthelorf/bcmr/releases/latest) 下載。

## 快速上手

```bash
# 複製檔案
bcmr copy document.txt backup/

# 遞迴複製目錄
bcmr copy -r projects/ backup/

# 移動檔案
bcmr move old_file.txt new_location/

# 確認後刪除
bcmr remove -r old_project/

# 乾跑 — 預覽操作但不執行
bcmr copy -r -n projects/ backup/
```

::: tip Shell 整合
可設定 shell 別名，讓 `cp`、`mv`、`rm`（或自訂前綴）自動使用 BCMR。詳見 [Shell 整合](/zh-Hant/guide/shell-integration)。
:::

## 下一步

- [Shell 整合](/zh-Hant/guide/shell-integration) — 替換或別名原生命令
- [設定](/zh-Hant/guide/configuration) — 自訂顏色、進度樣式和複製行為
- [CLI 參考](/zh-Hant/cli/commands) — copy、move 和 remove 的完整命令參考
