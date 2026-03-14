---
layout: home
hero:
  name: BCMR
  text: 更好的複製、移動、刪除
  tagline: 現代化、安全的檔案操作 CLI 工具 — 支援進度顯示、斷點續傳、完整性校驗和 SSH 遠端複製。
  image:
    src: /images/demo.gif
    alt: BCMR 示範
  actions:
    - theme: brand
      text: 快速開始
      link: /zh-Hant/guide/getting-started
    - theme: alt
      text: CLI 參考
      link: /zh-Hant/cli/
    - theme: alt
      text: GitHub
      link: https://github.com/Bengerthelorf/bcmr

features:
  - icon: "\U0001F4CA"
    title: 進度顯示
    details: 精美的 TUI 介面，支援漸變進度條、傳輸速度、ETA 和逐檔案進度追蹤。也提供純文字模式。
  - icon: "\U0001F504"
    title: 斷點續傳與校驗
    details: 支援透過 mtime、檔案大小或 BLAKE3 雜湊校驗續傳中斷的傳輸。複製後可驗證檔案完整性。
  - icon: "\U0001F310"
    title: 遠端複製 (SSH)
    details: 使用 SCP 風格語法透過 SSH 上傳和下載檔案，無需額外工具。
  - icon: "\u26A1"
    title: 預設高效能
    details: Reflink (寫時複製)、Linux copy_file_range、稀疏檔案偵測、流水線掃描+複製即時啟動。
  - icon: "\U0001F6E1\uFE0F"
    title: 安全操作
    details: 乾跑預覽、覆寫提示、正規表達式排除、透過暫存檔案+重新命名實現原子寫入。
  - icon: "\U0001F3A8"
    title: 完全可設定
    details: 自訂顏色漸變、進度條字元、邊框樣式，以及 reflink/sparse 預設值，均透過 TOML 設定。
---
