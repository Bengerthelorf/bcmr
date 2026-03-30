<div align="center">

<img src="img/icon.svg" width="128" height="128" alt="BCMR">

# BCMR

**Better Copy Move Remove — A modern, safe CLI tool for file operations with progress display, resume, and remote copy.**

[![Crates.io](https://img.shields.io/crates/v/bcmr?style=for-the-badge&color=blue)](https://crates.io/crates/bcmr)
&nbsp;
[![Documentation](https://img.shields.io/badge/Documentation-Visit_→-2ea44f?style=for-the-badge)](https://app.snaix.homes/bcmr/)
&nbsp;
[![Homebrew](https://img.shields.io/badge/Homebrew-Available-orange?style=for-the-badge)](https://github.com/Bengerthelorf/bcmr#install)

[中文版](README_zh.md)

<br>

![Demo](img/demo.gif)

<br>

### [📖 Read the Full Documentation →](https://app.snaix.homes/bcmr/)

Installation, shell integration, CLI reference, configuration, and more.

</div>

---

## Highlights

- 📊 **Progress Display** — Fancy TUI box with gradient bar, ETA, speed, per-file tracking. Plain text mode for logs and pipes
- 🔄 **Resume & Verify** — Crash-safe resume with session files and O(1) tail-block verification. Always-on BLAKE3 inline hashing for 2-pass verified copy
- 🌐 **Remote Copy (SSH)** — Upload and download via SSH. Binary `bcmr serve` protocol for fast transfers when both sides have bcmr, automatic fallback to legacy SCP
- ⚡ **Fast by Default** — Reflink (CoW), `copy_file_range` on Linux, sparse file detection, pipeline scan+copy, per-worker SSH connections for parallel transfers
- 🛡️ **Safe Operations** — Dry-run preview, overwrite prompts, regex exclusions, atomic writes with durable fsync (`F_FULLFSYNC` on macOS)
- 🔄 **Self-Update** — `bcmr update` to update in place; background update check on every run
- 🎨 **Configurable** — Custom color gradients, bar characters, border styles via TOML config

## Install

### Homebrew (macOS / Linux)

```bash
brew install Bengerthelorf/tap/bcmr
```

### Install Script

```bash
curl -fsSL https://app.snaix.homes/bcmr/install | bash
```

### Cargo

```bash
cargo install bcmr
```

### Pre-built Binaries

Download from [Releases](https://github.com/Bengerthelorf/bcmr/releases/latest) — available for Linux (x86_64/ARM64), macOS (Intel/Apple Silicon), Windows (x86_64/ARM64), and FreeBSD.

### From Source

```bash
git clone https://github.com/Bengerthelorf/bcmr.git
cd bcmr
cargo build --release
```

## Quick Start

```bash
# Copy files
bcmr copy document.txt backup/
bcmr copy -r projects/ backup/

# Move files
bcmr move old_file.txt new_location/
bcmr move -r old_project/ new_location/

# Remove files
bcmr remove -r old_project/
bcmr remove -i file1.txt file2.txt    # interactive

# Dry run — preview without changes
bcmr copy -r -n projects/ backup/

# Resume interrupted copy
bcmr copy -C large_file.iso /backup/

# Remote copy via SSH
bcmr copy local.txt user@host:/remote/
bcmr copy user@host:/remote/file.txt ./

# Parallel SCP transfers (4 workers)
bcmr copy -P 4 *.bin user@host:/backup/
bcmr copy -P 8 -r project/ user@host:/backup/
```

### Shell Integration

```bash
# Add to ~/.zshrc or ~/.bashrc:
eval "$(bcmr init zsh --cmd b)"    # creates bcp, bmv, brm

# Or replace native commands:
eval "$(bcmr init zsh --cmd '')"   # creates cp, mv, rm
```

> **Need help?** Check the [Getting Started](https://app.snaix.homes/bcmr/guide/getting-started) guide, or browse the full [Documentation](https://app.snaix.homes/bcmr/).

## Configuration

Create `~/.config/bcmr/config.toml`:

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
reflink = "auto"         # "auto" or "never"
sparse = "auto"          # "auto" or "never"

[scp]
parallel_transfers = 4   # default number of parallel SCP workers
compression = "auto"     # "auto", "force", or "off"

update_check = "notify"  # "notify", "quiet", or "off"
```

## Contributing

Issues and PRs welcome! See [GitHub Issues](https://github.com/Bengerthelorf/bcmr/issues).

## License

GPL-3.0 © [Zane Leong](https://github.com/Bengerthelorf)
