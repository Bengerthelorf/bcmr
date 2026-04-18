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

## Scope

bcmr is a **modern `cp` / `mv` / `rm` / `scp` replacement**, not an
`rsync` replacement. Concretely:

- **Single-file copy** (local or over SSH): has advantages over cp
  and scp — inline BLAKE3 integrity, $\mathcal{O}(1)$ resume
  verification, atomic crash-safe writes, wire-level Zstd/LZ4.
- **Many-file copy** (`bcmr copy -r`): on many-small-files workloads
  it's competitive with or faster than both cp and rsync (measured);
  on single large files it's within ~1.6× of cp.
- **What it is not**: a delta-sync tool. bcmr's content-addressed
  dedup matches **whole 4 MiB blocks** only — reliable for
  "re-upload the same artifact", useless for "the 3 MB of a 100 GB
  file that changed". rsync's rolling-checksum handles the latter;
  we don't.
- **Preservation parity with `rsync -a`**: partial. Mode, mtime, and
  xattrs are preserved; ACLs, BSD flags, and hardlink graphs are
  not yet.

See the [Internals](https://app.snaix.homes/bcmr/ablation/) page
for the measurements behind these claims.

---

## Highlights

- 📊 **Progress Display** — Fancy TUI box with gradient bar, ETA, speed, per-file tracking. Plain text mode for logs and pipes
- 🔄 **Resume & Verify** — Crash-safe resume with session files and O(1) tail-block verification. BLAKE3 inline hashing for 2-pass verified copy
- 🌐 **Remote Copy (SSH)** — Upload and download via SSH. Binary `bcmr serve` protocol for fast transfers when both sides have bcmr, automatic fallback to legacy SCP
- 🗜️ **Wire Compression** — `--compress={auto,zstd,lz4,none}`: per-block Zstd / LZ4 negotiated in the serve handshake, ~5× bandwidth on source-code text, auto-skip on incompressible blocks
- 🧠 **Content-Addressed Dedup** — uploads ≥ 16 MiB exchange block hashes first; the server only asks for blocks it doesn't already have in its local CAS. `BCMR_CAS_CAP_MB` bounds disk usage via LRU
- ⚡ **Parallel by Default** — `-j/--jobs` for local multi-file concurrency (default `min(CPU, 8)`); `-P/--parallel` for independent SSH connections; reflink (CoW), `copy_file_range`, `clonefile` on the kernel fast paths
- 🏷️ **Attribute Preservation** — `-p` carries mode, mtime, and extended attributes (Linux + macOS)
- 🛡️ **Safe Operations** — Dry-run preview, overwrite prompts, regex exclusions, atomic writes with durable fsync (`F_FULLFSYNC` on macOS)
- 🤖 **AI-Agent Friendly** — `--json` detaches to a background job writing NDJSON to `~/.local/share/bcmr/jobs/<id>.jsonl`; `bcmr status <id>` classifies into `scanning`/`running`/`done`/`failed`/`interrupted`
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

# Check differences between source and destination
bcmr check -r src/ dst/

# JSON output for AI agents / scripts
bcmr copy --json -r src/ dst/         # streaming NDJSON progress
bcmr check --json -r src/ dst/        # structured diff output
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

update_check = "off"     # "off" (default, no network), "quiet", or "notify"
```

## Contributing

Issues and PRs welcome! See [GitHub Issues](https://github.com/Bengerthelorf/bcmr/issues).

## Prior Art & Acknowledgments

bcmr stands on the shoulders of work that defined what "file
transfer over SSH" should feel like:

- **[mscp](https://github.com/upa/mscp)** (GPL-3.0) — the
  parallel-SSH-connections pattern that lets `bcmr serve
  --parallel N` scale past scp's single-stream crypto ceiling
  (see [Experiment 19](https://app.snaix.homes/bcmr/ablation/wire-protocol)).
  bcmr's implementation is an independent reimplementation of
  the concept in Rust, **not a derivative work** — no code was
  copied, only the architectural idea (open N independent SSH
  sessions, stripe files across them). Copyright protects
  expression, not algorithms (17 USC 102(b) and analogous
  provisions in other jurisdictions), so our Apache-2.0 license
  stands.
- **[HPN-SSH](https://www.psc.edu/hpn-ssh-home/)** — the
  enlarged-window and NONE-cipher patches that showed how
  constrained a stock OpenSSH data path really is. bcmr doesn't
  require HPN patches, but the diagnosis of "SSH single-stream
  crypto is the ceiling" is theirs.
- **`cp` / `mv` / `rm` / `rsync` / `scp`** — the user experience
  we benchmark against and try to earn a place alongside. The
  `docs/ablation` experiments cite specific comparisons where
  we win, lose, or draw.

## License

Apache-2.0 © [Zane Leong](https://github.com/Bengerthelorf)
