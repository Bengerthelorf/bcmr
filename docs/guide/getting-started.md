# Getting Started

BCMR (Better Copy Move Remove) is a modern CLI tool for file operations, written in Rust. It provides progress tracking, resume support, integrity verification, and remote copy over SSH.

## Requirements

- macOS (Intel or Apple Silicon), Linux (x86_64), or Windows (x86_64)

## Installation

::: code-group

```bash [Homebrew]
brew install Bengerthelorf/tap/bcmr
```

```bash [Install Script]
curl -fsSL https://bcmr.snaix.homes/ | bash
```

```bash [Cargo]
cargo install bcmr
```

```bash [From Source]
git clone https://github.com/Bengerthelorf/bcmr.git
cd bcmr
cargo build --release
# Binary at: ./target/release/bcmr
```

:::

Pre-built binaries (including Linux musl static) are available on the [Releases page](https://github.com/Bengerthelorf/bcmr/releases/latest).

## Quick Start

```bash
# Copy a file
bcmr copy document.txt backup/

# Recursively copy a directory
bcmr copy -r projects/ backup/

# Move files
bcmr move old_file.txt new_location/

# Remove with confirmation
bcmr remove -r old_project/

# Dry run — preview without changes
bcmr copy -r -n projects/ backup/
```

::: tip Shell Integration
Set up shell aliases to use `cp`, `mv`, `rm` (or custom prefixes) that automatically route through BCMR. See [Shell Integration](/guide/shell-integration).
:::

## Next Steps

- [Shell Integration](/guide/shell-integration) — Replace or alias native commands
- [Configuration](/guide/configuration) — Customize colors, progress style, and copy behavior
- [CLI Reference](/cli/commands) — Full command reference for copy, move, and remove
