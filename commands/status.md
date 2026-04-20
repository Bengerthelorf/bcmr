---
cmd: bcmr status
group: system
sig: "[job_id] [--json]"
desc: inspect state of --json background jobs (scanning / running / done / failed / interrupted)
tags: [stable]
order: 3
related: [bcmr copy, bcmr move, bcmr remove]
flags:
  - { f: "<job_id>", t: str,  d: "—",     x: "specific job id, or omit for list of recent jobs" }
  - { f: "--json",   t: bool, d: "false", x: "structured wrapper for scripts" }
example:
  - "764dee358ff4   running       78%"
  - "2c9a1f40d1ea   done         100%"
  - "3f8a5dd0c012   interrupted    —"
---

When `bcmr copy/move/remove` runs with `--json`, the command detaches to a
background process and writes progress as NDJSON to
`~/.local/share/bcmr/jobs/<id>.jsonl`. `bcmr status` reads those logs and
classifies each job:

- `scanning` — walking source tree, no byte moved yet
- `running` — in transfer phase
- `done` — finished with `status:"success"`
- `failed` — finished with `status:"error"` (error preserved in log)
- `interrupted` — pid gone, no terminal event written (crash / SIGKILL / OOM)

Logs older than 7 days are cleaned up when a new job starts.
