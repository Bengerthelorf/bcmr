---
cmd: bcmr doctor
group: system
sig: "[hosts]..."
desc: diagnose local + remote setup (ssh, config, bcmr presence, serve fast path)
tags: [stable]
order: 4
related: [bcmr deploy]
flags:
  - { f: "[hosts]...", t: "user@host", d: "—", x: "optional remote hosts to probe" }
  - { f: "--json", t: bool, d: "false", x: "emit a structured diagnostic report" }
  - { f: "--config <PATH>", t: path, d: "—", x: "inspect this file as the final configuration layer" }
example:
  - "bcmr 0.7.0 — diagnostic report"
  - "Local:"
  - "  ✓ config file: ~/.config/bcmr/config.toml (valid TOML)"
  - "lab:"
  - "  ✓ ssh: reachable as lab"
---

Probes local configuration layers, the jobs directory, terminal color
settings, and any named
remote hosts (ssh reachability, remote bcmr version, serve protocol
support). Each finding comes with a recommended fix — e.g. `bcmr deploy`
when a remote lacks the serve fast path. `--json` emits the report as a
structured envelope for scripts.
