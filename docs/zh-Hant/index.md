---
layout: home
hero:
  name: BCMR
  text: 更好的複製、移動、刪除
  tagline: 更現代的 cp / mv / scp — 每次複製自帶 BLAKE3 完整性校驗、崩潰後可續傳、本機和 SSH 遠端共用一個命令。
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
  - icon: ✅
    title: 預設完整性校驗
    details: "每次複製都在 write 過程中串流跑 BLAKE3。--verify 升級為完整的 2-pass 校驗 —— 不是 rsync --checksum 那樣的可選重掃。"
  - icon: 🔄
    title: 崩潰安全續傳
    details: "被打斷後同一條命令再跑一次就能續上 —— session 檔案、尾塊校驗、從停下的位置繼續。不需要 --partial --append-verify 組合咒語。"
  - icon: 🔗
    title: 本機 SSH 同一 CLI
    details: "bcmr copy a.txt /b/ 和 bcmr copy a.txt user@host:/b/ 是同一條命令、同一套 flag。不需要在 cp/scp/rsync 之間切上下文。"
  - icon: 🔐
    title: Direct-TCP 快速通道
    details: "可選的 AES-256-GCM over 直連 TCP 資料面，繞開 SSH 單流加密瓶頸。金鑰仍由 SSH 控制通道協商，身分驗證不變。"
  - icon: ⚡
    title: 預設高效能
    details: Reflink（寫時複製）、Linux copy_file_range、稀疏檔案偵測、掃描+複製流水線即時啟動。
  - icon: 🛡️
    title: 預設安全
    details: "透過暫存檔案+重新命名的原子寫入、持久 fsync（macOS 用 F_FULLFSYNC）、乾跑預覽、正規表達式排除。"
  - icon: 🤖
    title: 為人和 Agent 同時設計
    details: "TUI 即時進度與 ETA 供人閱讀。--json 脫離終端轉入後台，串流寫 NDJSON；bcmr status <id> 回傳狀態機。不怕關終端。"
  - icon: 🗜️
    title: 線路壓縮與去重
    details: "握手時協商每塊 LZ4/Zstd（原始碼類文字 ~5×），加上 ≥ 16 MiB 重傳的內容定址去重。"
---
