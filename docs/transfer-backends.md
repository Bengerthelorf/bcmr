---
title: Transfer Backends
section: guide
order: 12
---

bcmr 0.7.1 supports temporary connections, mixed sources, client-mediated
relay and subsystem-only SFTP accounts.

```bash
bcmr copy --ssh-port 2222 --ssh-identity ~/.ssh/temp file 'host:~/new/'
bcmr copy --bandwidth-limit 10MiB/s -P 4 'host:large' ./
bcmr copy local.txt 'A100:remote.txt' ./Downloads/
bcmr copy -r -V 'A100:results' '4090:~/archive/'
bcmr move --remote-backend=sftp -p 'restricted:report' ./Downloads/
bcmr copy 'scp://user@[::1]:2222/tmp/report%20one' ./
```

`--remote-backend=auto` probes remote execution once per connection identity and
uses SFTP when exec is unavailable and a real subsystem handshake succeeds.
`ssh` explicitly selects serve/shell; `sftp` connects only to the subsystem.
Authentication/network errors remain distinct. Global SSH options apply before
patterns, completion, deployment diagnostics and background execution.

The aggregate limiter counts delivered uncompressed application payload bytes
per second. `10KiB` is 10 × 1024 B/s; `10kB` is 10 × 1000 B/s. A bare positive
integer means B/s; an optional `/s` is accepted. Protocol/SSH overhead, metadata,
verification reads and validated resume-prefix reuse are excluded. All workers
and fan-out targets share a budget; same-filesystem move rename transfers no
payload. Capping disables automatic reflink/zero-copy/CAS shortcuts; forced
reflink with a cap is rejected.

Mixed/SFTP/relay plans validate all selections before destination writes and use
bounded buffers rather than a local whole-file intermediate. The composite
backend currently streams its selected leaves sequentially; `-P` continues to
control workers on the existing ordinary SSH/serve paths. Failed/canceled
transfers preserve move sources; selected source cleanup runs after all fan-out
targets succeed. A tree operation can leave completed destination files on failure.

SFTP supports inventory, `~/`, `*`, metadata, links, hashes and data I/O without
remote shell tools. Fresh regular files publish with negotiated OpenSSH
`hardlink@openssh.com`, avoiding the server's potentially racy v3 rename fallback.
Links publish with atomic SYMLINK creation. A server lacking that hardlink
extension cannot provide bcmr's no-clobber regular-file publication and is refused.
The [OpenSSH protocol extensions](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL)
describe the extension.

SFTP v3 cannot read/copy an existing entry's ACL/xattrs or conditionally exchange
its identity. Existing SFTP destination entries therefore cannot be force
replaced; choose a new path or use the SSH backend when exec is permitted.
Matching existing local/SSH resume prefixes can seed a staged result; SFTP
replacement remains subject to that security boundary. An already identical SFTP
file can be hash-validated and skipped with resume without replacement. Source metadata is
captured before move hashing. SFTP timestamps have one-second precision and
`~other-user` needs an absolute path. SFTP move guards use canonical parents,
content hashes and exposed metadata; the protocol has no inode/ctime identity,
so identical replacements and writes after the final check are not detectable.

Composite SSH/SFTP uploads reject `--sync` before writes: file fsync alone cannot
prove durable directory publication. Local Unix destinations retain strict
fsync support. Direct AES TCP and bcmr zstd/lz4 framing are unavailable on this
composite backend and explicit requests fail before transfer. OpenSSH compression
can be configured in the selected SSH config file. Ordinary serve keeps its
existing compression/direct transport support.

Move sources with colliding destination basenames are refused even with `-f`.
For delivered remote/composite leaves, cleanup quarantines the source and checks
that every recorded destination still holds the delivered file or link before
unlinking. A changed destination or an alias of the quarantined source cancels
cleanup. Interrupt monitoring covers copying and cleanup; JSON reports move
success only after cleanup finishes. These guards do not replace filesystem snapshots.

SFTP stages use canonical parents, private ownership markers and anchored payload
checks. Observed namespace changes retain recovery data and report its location.
Final `-p` file metadata uses a validated retained handle after guarded staging
cleanup; a new destination is briefly visible with private staging permissions.
Without `-p`, SFTP regular files retain private mode 0600. Exact replacements of
all protocol-visible guards and writes after the last check remain undetectable.
Forced reflink and sparse-always strategies are refused on composite streams.

Remote/composite `move -p` requires owner-readable final files and searchable
final directories so cleanup can verify the published namespace. Modes lacking
owner read (files) or owner search (directories) are refused before writes;
`copy -p` still supports restrictive final modes. Source ACLs do not transfer
through portable `-p`, so an ACL-readable source can otherwise become impossible
to validate at the destination.
