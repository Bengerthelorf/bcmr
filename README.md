# 🚀 BCMR (Better Copy Move Remove)

Making file operations simpler and more modern! BCMR is a command-line tool written in Rust that lets you elegantly copy, move, and remove files.

![Demo](img/demo.gif)

## 📥 Installation

### Using Cargo (Recommended)

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
- 🔄 Recursive Directory Operations - Handle entire folders with one command
- 🎨 Attribute Preservation - Keep timestamps, permissions, and more
- ⚡ Asynchronous I/O - Faster file operations
- 🛡️ Safe Confirmation System - Prevent accidental overwrites or deletions
- 🎭 File Exclusion - Flexibly ignore unwanted files
- 📊 Detailed Operation Info - Know exactly what's happening

## 📖 Detailed Usage Guide

### Copy Command

Basic syntax:

```bash
bcmr copy [options] <source> <destination>
```

Available options:

- `-r, --recursive`: Copy directories recursively
- `--preserve`: Preserve file attributes (timestamps, permissions)
- `-f, --force`: Force overwrite existing files
- `-y, --yes`: Skip confirmation when using force
- `--exclude=<pattern>`: Exclude files matching pattern (comma-separated)

Examples:

```bash
# Copy a single file
bcmr copy document.txt backup/

# Recursively copy a directory
bcmr copy -r projects/ backup/

# Copy with attribute preservation
bcmr copy --preserve important.conf /etc/

# Force overwrite without prompting
bcmr copy -f -y source.txt destination.txt

# Copy with exclusions
bcmr copy -r --exclude=.git,*.tmp src/ dest/
```

### Move Command

Basic syntax:

```bash
bcmr move [options] <source> <destination>
```

Available options:

- `-r, --recursive`: Move directories recursively
- `--preserve`: Preserve file attributes
- `-f, --force`: Force overwrite existing files
- `-y, --yes`: Skip overwrite confirmation
- `--exclude=<pattern>`: Exclude matching files

Examples:

```bash
# Move a single file
bcmr move old_file.txt new_location/

# Recursively move a directory
bcmr move -r old_project/ new_location/

# Force move with attribute preservation
bcmr move -f --preserve config.json /etc/

# Move with exclusions
bcmr move -r --exclude=node_modules,*.log project/ new_place/
```

### Remove Command

Basic syntax:

```bash
bcmr remove [options] <path1> [path2 ...]
```

Available options:

- `-r, --recursive`: Recursively remove directories
- `-f, --force`: Force removal without confirmation
- `-i, --interactive`: Prompt before each removal
- `-v, --verbose`: Show detailed removal process
- `-d`: Remove empty directories
- `--exclude=<pattern>`: Exclude matching files

Examples:

```bash
# Remove a single file
bcmr remove unnecessary.txt

# Recursively remove a directory
bcmr remove -r old_project/

# Interactive removal of multiple files
bcmr remove -i file1.txt file2.txt file3.txt

# Remove empty directory
bcmr remove -d empty_directory/

# Force recursive removal with verbose output
bcmr remove -rf -v outdated_folder/

# Remove with exclusions
bcmr remove -r --exclude=*.important,*.backup trash/
```

## ⚙️ Shell Configuration

For convenient usage of BCMR, you can set up these aliases in your shell:

```bash
# Add to ~/.bashrc or ~/.zshrc
alias cp='bcmr copy'
alias mv='bcmr move'
alias rm='bcmr remove'
```

## 🤝 Contributing

Issues and PRs are welcome! Whether it's bug reports or feature suggestions, we appreciate all contributions.

1. Fork the project
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 🐛 Bug Reports

If you find a bug or have any suggestions, please submit them to the GitHub Issues page. When reporting, please include:

- BCMR version used
- Operating system details
- Steps to reproduce the issue
- Expected behavior
- Actual behavior

## 📝 License

GPL-3.0 © [Zane Leong](https://github.com/Bengerthelorf)