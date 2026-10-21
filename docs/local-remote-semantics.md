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

## Overwrites and moves

Ordinary file copies require `-f` to replace an existing target. Use `-f -y`
for explicit unattended replacement. Resume/append retains its separate prefix
validation rules. Fresh legacy downloads now stage and publish after successful
reading and optional verification; failed downloads preserve the old target.

Fan-out dry-run never removes sources. Remote/fan-out moves reject `--exclude`
until cleanup can preserve all excluded descendants: copy with excludes, verify,
then remove only the intended originals.

## Remaining differences

There is no claim of complete local/remote equivalence:

- Remote directory attributes under `-p` and recursive symlink traversal remain
  incomplete compared with local operations.
- Remote exclusion handling does not consistently prune excluded ancestor
  directories, and legacy uploads do not share every top-level exclusion rule.
- Local moves usually use rename; remote moves copy then delete. Directory
  merging and failure atomicity therefore differ. Local rename also retains a
  pre-existing check/rename race against concurrently created destinations.
- A forced legacy upload still streams into the visible remote file; interruption
  can leave changed or partial bytes. Serve uploads and fresh staged downloads
  have stronger publication guarantees.
- Some remote overwrite confirmation details differ; `-f -y` is the explicit
  unattended form.
- Windows cannot represent every Unix filename. Downloads reject native drive,
  traversal and device-name components instead of writing outside the requested
  directory.

Remote-to-remote and SFTP-only endpoints remain capability gaps. See
[scp compatibility](./scp-compatibility.md).
