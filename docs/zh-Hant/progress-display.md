---
title: 進度顯示
section: guide
order: 4
locale: zh-Hant
---

BCMR 為所有檔案操作提供五種明確的進度顯示模式。可使用
`--progress=auto|tui|inline|plain|off`，或在設定中使用同名的 `progress.style`。

## Auto 模式（預設）

Auto 會依輸出環境選擇最豐富且安全的顯示方式：

- 有足夠空間且支援顏色的 TTY 使用 TUI
- 設定 `NO_COLOR` 或終端較小時使用無色 inline
- 管道、重新導向和 `TERM=dumb` 使用穩定的 plain 最終摘要

## TUI 模式

TUI 介面包含：

- 帶顏色漸變的總進度條
- 傳輸速度和 ETA
- 目前檔案名稱和逐檔案進度條
- 項目計數（用於刪除操作）
- 掃描指示器（流水線模式即時顯示已發現的檔案數）

支援 Ctrl+C（清理暫存檔案後結束）和 Ctrl+Z（Unix 上暫停/恢復）。

## Inline 模式

無顏色的 3 行即時顯示，適合不希望使用完整 TUI 的終端：

```
Copying: [=========-----------] 45%
12.34 MiB / 27.00 MiB | 5.67 MiB/s | ETA: 00:03
File: largefile.zip [====----] 50%
```

## Plain 與 Off 模式

`plain` 只輸出穩定的最終摘要，不使用游標控制，適合日誌和管道。`off`
完全關閉進度；`-q` / `--quiet` 是方便的快捷方式。

## 流水線掃描

當不需要覆寫提示或乾跑時，BCMR 使用流水線模式 — 在目錄掃描進行的同時立即開始複製。進度顯示會展示掃描動畫，檔案計數即時更新，掃描完成後切換到正常進度檢視。

## 自訂

詳見 [設定](/zh-Hant/guide/configuration) 了解顏色漸變、進度條字元和邊框樣式。
