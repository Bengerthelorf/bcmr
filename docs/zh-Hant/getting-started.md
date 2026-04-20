---
title: 快速開始
section: guide
order: 1
locale: zh-Hant
---

BCMR (Better Copy Move Remove) 是一個用 Rust 編寫的現代化檔案操作 CLI
工具，提供進度追蹤、斷點續傳、完整性校驗和 SSH 遠端複製功能。

如果還沒裝，先看 [**安裝**](/install) —— 本頁假設 `bcmr` 已經在 `$PATH`
上了。

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

:::callout[Shell 整合]{kind="info"}
可設定 shell 別名，讓 `cp`、`mv`、`rm`（或自訂前綴）自動使用
bcmr。詳見 [Shell 整合](/zh-Hant/guide/shell-integration)。
:::

## 下一步

- [Shell 整合](/zh-Hant/guide/shell-integration) — 替換或別名原生命令
- [設定](/zh-Hant/guide/configuration) — 顏色、進度樣式、複製行為
- [遠端複製](/zh-Hant/guide/remote-copy) — SSH 與 direct-tcp 快速通道
- [CLI 參考](/commands) — 所有子命令 / 旗標，可搜尋
