---
cmd: bcmr deploy
group: system
sig: <targets>... [--path <path>] [--sudo]
desc: install or upgrade bcmr on remote hosts, with per-host verification and a final summary
tags: [stable]
order: 2
related: [bcmr update]
flags:
  - { f: "<targets>...", t: "SSH alias|user@host", d: "—", x: "one or more hosts, processed independently in order" }
  - { f: "--path", t: path, d: "~/.local/bin/bcmr", x: "installation path to check, upgrade, and verify on every host" }
  - { f: "--sudo", t: bool, d: "false", x: "remote sudo; terminal prompts for a password, non-interactive/JSON requires sudo -n" }
  - { f: "--config <PATH>", t: path,        d: "—",                    x: "layer this file over discovered configuration" }
example:
  - "$ bcmr deploy A100 4090 --path /usr/bin/bcmr --sudo"
  - "A100: installed"
  - "4090: skipped (already current)"
  - "1 installed, 1 skipped, 0 failed"
---

Enables the [serve protocol](/guide/remote-copy#serve-protocol) for direct-tcp
fast-path transfers. Each host is checked independently. Matching platforms
receive the local executable; other platforms download the matching asset
from the release with the local version number. That release must exist.

```sh
bcmr deploy iAu --path /usr/bin/bcmr --sudo
bcmr deploy A100 4090
bcmr deploy A100 4090 --path /usr/bin/bcmr --sudo
```

The default installation path is `~/.local/bin/bcmr`. The selected path,
including an explicit `--path`, determines whether installation is needed.
A matching version on a different PATH entry cannot cause it to be skipped.
After installation, the selected executable must report the expected version.
Already-current targets are skipped without requiring sudo authentication.

Both installed and skipped results display the installation path/version and
the path/version selected by the remote's non-interactive SSH PATH. If PATH
selects another executable, the result warns about shadowing. Use `--path`
to upgrade that installation, with `--sudo` if it requires elevated permissions.

For `--sudo`, a human run with terminal stdin and stderr uses native SSH PTY
handling for ordinary sudo password authentication. Password entry is handled
by SSH and sudo. Without a terminal, or with `--json`, installation uses
`sudo -n` and fails clearly when passwordless mkdir/test/install is not permitted.
SSH authentication continues to use the configured keys and batch mode.

Hosts run sequentially so sudo prompts identify one host at a time. A host's
connection, platform, transfer, permission, or verification failure does not
stop later hosts. The final summary lists each host and the installed, skipped,
and failed counts. Any failure gives a nonzero exit status; rerunning the
same command skips hosts whose selected installation is already current.
