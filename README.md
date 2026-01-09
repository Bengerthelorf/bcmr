# 🚀 BCMR (Better Copy Move Remove)

[English](README.md) | [中文](README_zh.md)

Making file operations simpler and more modern! BCMR is a command-line tool written in Rust that lets you elegantly copy, move, and remove files.

![Demo](img/demo.gif)

## 📥 Installation

### Using Install Script (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Bengerthelorf/bcmr/main/install.sh | bash
```

### Using Cargo

```bash
cargo install bcmr
```

### Building from Source

```bash
git clone https://github.com/Bengerthelorf/bcmr
cd bcmr
cargo build --release
```

The compiled binary will be available at `target/release/bcmr`.

## ✨ Features

- 🎯 Real-time Progress Bar - No more guessing how long it'll take
- ⏳ ETA Display - See estimated time remaining
- 🔄 Recursive Directory Operations - Handle entire folders with one command
- 🎨 Attribute Preservation - Keep timestamps, permissions, and more
- ⚡ Asynchronous I/O - Faster file operations
- 🛡️ Safe Confirmation System - Prevent accidental overwrites or deletions
- 🎭 Regex File Exclusion - Flexibly ignore unwanted files using Regular Expressions
- 🔍 Dry Run Mode - Preview operations without making changes
- 📊 Detailed Operation Info - Know exactly what's happening
- 🔌 Shell Integration - Customize command names with flexible prefixes
- 🎮 Two Progress Display Modes - Plain text (default) or fancy TUI display

## 📖 Detailed Usage Guide

### Shell Integration

BCMR provides flexible shell integration similar to zoxide. You can customize command names with prefixes or replace native commands.

Basic syntax:

```bash
bcmr init [shell] [options]
```

Available options:

- `--cmd <prefix>`: Set command prefix (e.g., 'b' creates bcp, bmv, brm)
- `--no-cmd`: Don't create command aliases
- `--path <path>`: Add directory to PATH

Examples:

```bash
# Add to your ~/.bashrc or ~/.zshrc:
# Use custom prefix (creates testcp, testmv, testrm)
eval "$(bcmr init zsh --cmd test)"

# Replace native commands (creates cp, mv, rm)
eval "$(bcmr init zsh --cmd '')"

# Use 'b' prefix (creates bcp, bmv, brm)
eval "$(bcmr init bash --cmd b)"
```

Supported shells:

- Bash
- Zsh
- Fish

### Copy Command

Basic syntax:

```bash
bcmr copy [options] <source>... <destination>
```

Available options:

- `-r, --recursive`: Copy directories recursively
- `--preserve`: Preserve file attributes (timestamps, permissions)
- `-f, --force`: Force overwrite existing files
- `-y, --yes`: Skip confirmation when using force
- `-n, --dry-run`: Preview operation without making changes
- `--exclude=<pattern>`: Exclude files matching Regex pattern (comma-separated)
- `--fancy-progress`: Use fancy TUI progress display (default is plain text)

Examples:

```bash
# Copy a single file
bcmr copy document.txt backup/

# Copy multiple files (Shell Globbing works!)
bcmr copy *.txt *.md backup/

# Recursively copy a directory
bcmr copy -r projects/ backup/

# Dry run (preview what would be copied)
bcmr copy -r -n projects/ backup/

# Copy with attribute preservation
bcmr copy --preserve important.conf /etc/

# Force overwrite without prompting
bcmr copy -f -y source.txt destination.txt

# Copy with Regex exclusions (exclude .git folder and .tmp files)
bcmr copy -r --exclude="\.git","\.tmp$" src/ dest/
```

### Move Command

Basic syntax:

```bash
bcmr move [options] <source>... <destination>
```

Available options:

- `-r, --recursive`: Move directories recursively
- `--preserve`: Preserve file attributes
- `-f, --force`: Force overwrite existing files
- `-y, --yes`: Skip overwrite confirmation
- `-n, --dry-run`: Preview operation without making changes
- `--exclude=<pattern>`: Exclude matching files (Regex)
- `--fancy-progress`: Use fancy TUI progress display (default is plain text)

Examples:

```bash
# Move a single file
bcmr move old_file.txt new_location/

# Move multiple files
bcmr move file1.txt file2.txt new_location/

# Recursively move a directory
bcmr move -r old_project/ new_location/

# Dry run
bcmr move -n old_project/ new_location/

# Move with exclusions (Regex)
bcmr move -r --exclude="^node_modules","\.log$" project/ new_place/
```

### Remove Command

Basic syntax:

```bash
bcmr remove [options] <path>...
```

Available options:

- `-r, --recursive`: Recursively remove directories
- `-f, --force`: Force removal without confirmation
- `-i, --interactive`: Prompt before each removal
- `-v, --verbose`: Show detailed removal process
- `-d`: Remove empty directories
- `-n, --dry-run`: Preview operation without making changes
- `--exclude=<pattern>`: Exclude matching files (Regex)
- `--fancy-progress`: Use fancy TUI progress display (default is plain text)

Examples:

```bash
# Remove a single file
bcmr remove unnecessary.txt

# Remove multiple files (Globbing)
bcmr remove *.log

# Recursively remove a directory
bcmr remove -r old_project/

# Dry run (safe check)
bcmr remove -r -n potentially_important_folder/

# Interactive removal of multiple files
bcmr remove -i file1.txt file2.txt file3.txt

# Remove with exclusions (Regex)
bcmr remove -r --exclude="\.important$","\.backup$" trash/
```

### Progress Display Modes

BCMR offers two progress display modes:

1. **Plain Text Mode (Default)**: Simple text-based progress bars that work in any terminal
2. **Fancy TUI Mode**: Rich terminal UI with enhanced visual elements and gradients

#### Fancy Mode Configuration

You can fully customize the fancy progress bar by creating a configuration file at `~/.config/bcmr/config.toml`:

```toml
[progress]
# Set the style to "fancy" to potentially make it default in future versions (currently requires flag)
style = "fancy"

[progress.theme]
# Define a gradient for the progress bar (Hex colors) - Default is a Morandi purple gradient
bar_gradient = ["#CABBE9", "#7E6EAC"] 
bar_complete_char = "█"
bar_incomplete_char = "░"
text_color = "reset"       # "reset" adapts to your terminal background
border_color = "#9E8BCA"
title_color = "#9E8BCA"

[progress.layout]
# Options: rounded, double, heavy, single
box_style = "rounded"
```

Use `--fancy-progress` flag to enable the fancy TUI mode for a more visually appealing experience.

## 📝 License

GPL-3.0 © [Zane Leong](https://github.com/Bengerthelorf)
