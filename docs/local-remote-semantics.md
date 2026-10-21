---
title: Local and Remote Behavior
section: guide
order: 10
---

This describes development source as of 2026-10-21, after published 0.7.0.

## Destination paths

These rules apply to local copy, uploads and downloads, including the legacy
SSH fallback:

| Destination | Result |
|---|---|
| Existing directory | Keep the source basename inside it. |
| Missing path ending in `/` | Create the directory and keep the source basename. |
| Bare missing path, one source | Use that path as the new file or directory name. |
| Multiple sources | Require an existing directory or an explicit trailing `/`. |
| Full file path with missing ancestors | Create the parent directories. |
| Existing regular file followed by `/` | Fail without changing it. |
| Dry-run | Compute the same paths without creating directories or removing sources. |

```bash
bcmr copy report.pdf new/test/                  # new/test/report.pdf
bcmr copy report.pdf 'host:~/new/test/'          # ~/new/test/report.pdf
bcmr copy 'host:report.pdf' new/test/            # new/test/report.pdf
bcmr copy -r project renamed                    # renamed/<project contents>
bcmr copy -r project new/                       # new/project/<contents>
```

Source trailing slashes do not implicitly request an rsync-style contents-only
copy. Source stars resolve once, before destination planning.

## Unified behavior

- `-p` finalizes root, nested and empty directory permissions and times after content, deepest first.
- Exclusions match source spellings and source-relative paths, prune ancestor subtrees, and apply to totals, preview, directory creation, payloads and move cleanup.
- Copy follows file and directory links by default; `--no-deref` preserves links locally and in both remote directions, including dangling links. When following links, dangling targets and cycles fail explicitly; excluded links are not followed. Serve root confinement applies to followed descendants.
- Move preserves link identity and never removes link targets. Existing directory targets merge with the same leaf overwrite rules. Native no-replace rename protects concurrent local targets.
- Forced overwrite requires the same approval locally and remotely. Use `-f -y` for unattended replacement. Declining cancels move cleanup; dry-run never requests overwrite approval.
- Fresh legacy SSH uploads and downloads stage content before publication. Size, verification and attributes complete before replacing the final file. Serve `-p`/`-V` uploads also finalize a private stage first.
- Remote and fan-out moves support exclusions. Cleanup removes only the captured selected entries, retains excluded/new content, checks source identity and metadata, and reports a retained recovery object if an unsafe restoration cannot be completed.

## Intentional boundaries

- A same-filesystem rename can be atomic. A cross-filesystem or cross-host tree move is copy plus cleanup, not one transaction. Completed target files can remain on failure, and cleanup can fail after earlier entries were removed.
- Captured identity and metadata plus private quarantine protect source namespace replacements. A noncooperating writer holding an open fd can still modify the inode after the last check; Hardlink cleanup must refresh shared inode ctime after its own rename/unlink; timestamp-preserving writes in that bounded restat interval are also indistinguishable from our change. Transaction isolation requires a snapshot or cooperating locks.
- Validated resume and append intentionally operate on partial files in place; they do not promise a rollback of old content.
- Portable `-p` covers ordinary permission bits and timestamps within platform/backend precision. POSIX shell timestamp tools have second precision. Source ownership, special bits and arbitrary ACL/xattr replication are not cross-platform promises. Existing destination security metadata is preserved where supported; unsafe publication fails closed.
- The portable copy phase does not reproduce source hardlink topology; native rename retains it. This requires an archive-style feature rather than changing ordinary copy semantics.
- Remote inventory/download names and preserved link-target strings require UTF-8. Selected unrepresentable remote entries fail explicitly, while excluded entries are pruned before name validation. Raw byte-name portability is not promised by the existing upload path interfaces.
- Windows names and symlink privileges cannot represent every Unix source. Unsafe native components are rejected rather than silently renamed.
- An explicit serve `--root` remains an access boundary for followed links. Linux/macOS LIST binds checked targets through directory handles; other platforms retain path-based checks and do not promise immunity to hostile concurrent ancestor replacement.
- Link-preserving remote operations use SSH auxiliary operations and exact-path ordinary payloads for compatibility. Their throughput and durability capabilities differ from serve.

Unsupported `--sync` combinations refuse before writes; see the remote-copy guide. The remaining SSH CLI, relay and SFTP-only capabilities are assessed in [scp compatibility](./scp-compatibility.md).
