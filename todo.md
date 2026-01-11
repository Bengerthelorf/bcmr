# Project Roadmap

A prioritized list of improvements, refactoring, and new features to be implemented within the v0.3.x lifecycle.

## Code Quality & Stability

- [x] **Unified Traversal Logic**
  - Extract directory walking and regex exclusion logic from `copy.rs`, `move.rs`, and `remove.rs` into a shared module.
  - *Goal*: Reduce code duplication and ensure consistent behavior across commands.

- [ ] **Centralized Signal Handling**
  - Create a global implementation to manage SIGINT (Ctrl+C) and SIGTERM.
  - Ensure robust cleanup of partially copied/moved files upon interruption.

- [x] **Custom Error Types**
  - Replace generic `anyhow::Result` with specific `thiserror` enums (e.g., `PermissionDenied`, `TargetExists`).
  - Provide actionable user tips based on error types.

## Performance & Concurrency

- [ ] **Parallel File Operations**
  - Implement a concurrency model (e.g., Worker Pool or Async Stream) to process multiple small files simultaneously.
  - Limit concurrency usage based on system resources.
  - *Goal*: Improve performance for directories with large numbers of small files.

- [x] **Reflink / Copy-on-Write (CoW)**
  - Implement support for `reflink` on compatible file systems (APFS, Btrfs, XFS).
  - *Goal*: Enable instantaneous file copies without additional disk usage.

- [ ] **Background & Suspend Support (SIGTSTP/SIGCONT)**
  - Handle `Ctrl+Z` (SIGTSTP) to suspend execution gracefully (stop TUI rendering).
  - Handle `fg/bg` (SIGCONT) to resume.
    - If resumed in foreground: Restore TUI.
    - If resumed in background: Continue operation silently (no TUI).

## User Experience & Safety

- [ ] **Trash / Recycle Bin Support**
  - Update `remove` command to move files to the system Trash by default.
  - Add `--permanent` flag for permanent deletion.

- [ ] **Smart Conflict Resolution**
  - Enhance the overwrite prompt to support:
    - `[S]kip`: Skip the current file.
    - `[R]ename`: Auto-rename the target.
    - `[A]pply to all`: Apply the choice to all subsequent conflicts.

- [x] **Resume & Verification**
  - Implement `--verify` flag for hash-based integrity checking.
  - Support resumable copy operations.

- [ ] **Wake Lock Support**
  - Prevent system sleep during long running operations (copy/move).
  - *Reference*: `keepawake` crate used in `mc-cli`.

- [ ] **IO Throttling**
  - Add explicit bandwidth limiting flags (e.g., `--bwlimit`).
  - *Reference*: `bcmr` has `TestMode` internal throttling but no user-facing flag for it yet.
