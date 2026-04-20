---
layout: home
hero:
  name: BCMR
  text: Better Copy Move Remove
  tagline: A modern cp, mv, and scp — with BLAKE3 integrity built in, crash-safe resume, and one CLI for local and SSH transfers.
  image:
    src: /images/demo.gif
    alt: BCMR Demo
  actions:
    - theme: brand
      text: Get Started
      link: /guide/getting-started
    - theme: alt
      text: CLI Reference
      link: /cli/
    - theme: alt
      text: View on GitHub
      link: https://github.com/Bengerthelorf/bcmr

features:
  - icon: ✅
    title: Integrity by Default
    details: "Every copy streams through BLAKE3 during the write. --verify promotes that to a full 2-pass checksum round-trip — not an opt-in rescan like rsync's --checksum."
  - icon: 🔄
    title: Crash-Safe Resume
    details: "Interrupt a bcmr copy and run the same command again — session file, tail-block verify, continue where it stopped. No --partial --append-verify incantation."
  - icon: 🔗
    title: One CLI, Local or SSH
    details: "bcmr copy a.txt /b/ and bcmr copy a.txt user@host:/b/ are the same command with the same flags. No cp/scp/rsync context switch."
  - icon: 🔐
    title: Direct-TCP Fast Path
    details: "Optional AES-256-GCM data plane over direct TCP bypasses SSH's single-stream crypto ceiling on LAN. Keys negotiated via the SSH control channel."
  - icon: ⚡
    title: Fast by Default
    details: Reflink (copy-on-write), copy_file_range on Linux, sparse file detection, and pipeline scan+copy for immediate start.
  - icon: 🛡️
    title: Safe by Default
    details: "Atomic writes via temp file + rename, durable fsync (F_FULLFSYNC on macOS), dry-run preview, and regex exclusions."
  - icon: 🤖
    title: Built for Humans and Agents
    details: "TUI with per-file progress for humans. --json detaches to a background job streaming NDJSON; bcmr status <id> returns state. Survives terminal close."
  - icon: 🗜️
    title: Wire Compression & Dedup
    details: "Per-block LZ4/Zstd negotiated in the serve handshake (~5× on source text), plus content-addressed dedup for repeat uploads ≥ 16 MiB."
---
