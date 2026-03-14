---
layout: home
hero:
  name: BCMR
  text: Better Copy Move Remove
  tagline: A modern, safe CLI tool for file operations — with progress display, resume, verification, and remote copy via SSH.
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
  - icon: 📊
    title: Progress Display
    details: Fancy TUI box with gradient progress bar, ETA, speed, and per-file tracking. Or plain text mode for logs and pipes.
  - icon: 🔄
    title: Resume & Verify
    details: Resume interrupted transfers with mtime, size, or strict BLAKE3 hash checks. Verify integrity after copy.
  - icon: 🌐
    title: Remote Copy (SSH)
    details: Upload and download files over SSH with SCP-like syntax. No extra tools needed.
  - icon: ⚡
    title: Fast by Default
    details: Reflink (copy-on-write), copy_file_range on Linux, sparse file detection, and pipeline scan+copy for immediate start.
  - icon: 🛡️
    title: Safe Operations
    details: Dry-run preview, overwrite prompts, regex exclusions, atomic writes via temp file + rename.
  - icon: 🎨
    title: Fully Configurable
    details: Custom color gradients, bar characters, border styles, and reflink/sparse defaults via TOML config.
---
