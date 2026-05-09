---
title: --json NDJSON Event Schema
section: internals
order: 8
---

`bcmr --json` and the background-job log files (`~/.local/share/bcmr/jobs/<job_id>.jsonl`) emit newline-delimited JSON. This page is the contract for consumers — CI scripts, agent tools, dashboards reading `bcmr status`. Everything here is observed-stable; fields not listed are not yet stable and may change without notice.

## Event types

Every line carries a `type` field discriminating the event, *except* the very first line of a background-job log, which is the **header** (`{job_id, pid, log}`). Streaming parsers should special-case line 1 for now; a future schema bump will add `"type":"submitted"` to the header so dispatch can be uniform.

The current taxonomy:

| `type`     | Where             | When emitted                                       |
| ---------- | ----------------- | -------------------------------------------------- |
| (header)   | log file, line 1  | Once, at job spawn                                 |
| `progress` | stdout / log file | At least every 200 ms during transfer / scan      |
| `result`   | stdout / log file | Once, at the end of a copy / move / remove run    |

Foreground non-operation commands (`bcmr check --json`) emit a single result-shaped object on stdout instead — see the **`bcmr check`** section below.

## Header line

Emitted only for `--json` operations that detach to a background job (`bcmr copy`, `bcmr move`, `bcmr remove`). Foreground `--json` runs do not emit it.

```json
{"job_id":"d912cf1b2c4","pid":45766,"log":"/Users/.../jobs/d912cf1b2c4.jsonl"}
```

| Field    | Type   | Notes                                                                |
| -------- | ------ | -------------------------------------------------------------------- |
| `job_id` | string | Stable for the job's lifetime. Used by `bcmr status <job_id>`.       |
| `pid`    | number | OS process ID of the detached worker.                                |
| `log`    | string | Absolute path to the per-job NDJSON log file.                        |

The same object is also written to `<job_id>.jsonl` on disk and printed to stdout when the parent process detaches.

## `progress` events

Emitted at most every `200 ms` during operations. Always present.

```json
{"type":"progress","operation":"Uploading (serve)","bytes_done":1048576,"bytes_total":5242880,"percent":20.0,"speed_bps":4194304,"eta_secs":1,"file":"data.bin","file_size":5242880,"file_progress":1048576,"items_done":0,"items_total":1,"scanning":false}
```

| Field            | Type     | Stable? | Notes                                                                                                |
| ---------------- | -------- | ------- | ---------------------------------------------------------------------------------------------------- |
| `type`           | string   | ✅       | Always `"progress"`.                                                                                 |
| `operation`      | string   | ⚠️      | Human-readable phase. Today: `Copying`, `Moving`, `Removing`, `Uploading (serve)`, `Uploading (legacy)`, `Downloading (serve)`, `Downloading (legacy)`, `Scanning`, `Verifying`. Treat as opaque label, not enum. |
| `bytes_done`     | number   | ✅       | Bytes transferred so far across all files in the run.                                                |
| `bytes_total`    | number   | ✅       | Best estimate after scan completes; may be 0 mid-scan.                                               |
| `percent`        | number   | ✅       | `bytes_done / bytes_total * 100` (0.0 if total unknown). Float.                                      |
| `speed_bps`      | number   | ✅       | **Bytes per second**, not bits. Recent-window average.                                               |
| `eta_secs`       | number   | ✅       | Estimated seconds remaining. Omitted (`null` / absent) when speed is too unstable to estimate.       |
| `file`           | string   | ✅       | Current per-file display name (basename).                                                            |
| `file_size`      | number   | ✅       | Total bytes of `file`.                                                                               |
| `file_progress`  | number   | ✅       | Bytes transferred for `file` so far.                                                                 |
| `items_done`     | number   | ✅       | Files / dir-entries finished. Omitted (`null` / absent) for single-file ops.                         |
| `items_total`    | number   | ✅       | Total items expected. Omitted while scanning is in progress.                                         |
| `scanning`       | bool     | ✅       | `true` during the initial walk before any byte is transferred. Flips to `false` once execution starts. |

**Notes for consumers:**

- For very short transfers, no `progress` event may fire at all — only the header (if backgrounded) and the terminal `result` event. Don't wait for at least one `progress` before declaring "started".
- `speed_bps` is bytes per second despite the suffix; the unit is fixed for backwards compatibility.
- Field order is not guaranteed; always parse by name.
- Unknown fields may appear in future versions — ignore them.

## `result` events

Exactly one terminal event per run.

Success:

```json
{"type":"result","status":"success","operation":"Uploading (serve)","bytes_total":5242880,"duration_secs":1.247,"avg_speed_bps":4205937}
```

Error:

```json
{"type":"result","status":"error","operation":"Uploading (serve)","bytes_total":1048576,"duration_secs":0.521,"error":"connection closed by peer"}
```

| Field             | Type     | Stable? | Notes                                                                          |
| ----------------- | -------- | ------- | ------------------------------------------------------------------------------ |
| `type`            | string   | ✅       | Always `"result"`.                                                             |
| `status`          | string   | ✅       | `"success"` or `"error"`.                                                      |
| `operation`       | string   | ⚠️      | Same caveat as `progress.operation`; treat as opaque label.                    |
| `bytes_total`     | number   | ✅       | Bytes actually transferred (may be less than the planned total on error).      |
| `duration_secs`   | number   | ✅       | Wall-clock seconds for the run. Float.                                         |
| `avg_speed_bps`   | number   | ✅       | **Bytes per second**. Omitted on error.                                        |
| `bytes_skipped`   | number   | ✅       | Bytes skipped via `--append` / `--update`. Omitted when `0`.                   |
| `verified`        | bool     | ✅       | `true` when the run was invoked with `-V/--verify`. Omitted when `false`.      |
| `error`           | string   | ✅       | Human-readable error. Present only when `status="error"`.                      |

## `bcmr check --json`

`bcmr check` runs in the foreground (no background job, no header line). It emits a single JSON object — not an NDJSON stream:

```json
{
  "command": "check",
  "status": "success",
  "in_sync": false,
  "added": [{"path":"new.txt","size":42,"src_size":42,"is_dir":false}],
  "modified": [{"path":"changed.bin","src_size":100,"dst_size":100,"is_dir":false}],
  "missing": [{"path":"old.txt","size":10,"dst_size":10,"is_dir":false}],
  "summary": {"added":1,"modified":1,"missing":1,"total_bytes":52}
}
```

| Field         | Type        | Notes                                                                       |
| ------------- | ----------- | --------------------------------------------------------------------------- |
| `command`     | string      | Always `"check"` for this output type.                                      |
| `status`      | string      | `"success"` or `"error"`.                                                   |
| `in_sync`     | bool        | `true` iff `added`, `modified`, and `missing` are all empty.                |
| `added`       | array       | Files present in source but not in dest. Omitted when empty.                |
| `modified`    | array       | Files present in both with content drift. Omitted when empty.               |
| `missing`     | array       | Files present in dest but not in source. Omitted when empty.                |
| `summary`     | object      | Always present. Counts and `total_bytes` of added + modified bytes.         |
| `error`       | string      | Present only on `status="error"`.                                           |
| `error_kind`  | string      | Present only on `status="error"`. Categorical: `invalid_input`, `io`, etc.  |

`FileDiff` entries always carry `path` (relative) and `is_dir`. The size fields are situational: `src_size` for added entries, `dst_size` for missing entries, both for modified entries when they differ. `size` is a legacy convenience that mirrors the active size field.

Exit code is `0` when `in_sync=true`, `1` when not, `2` on error.

## Error events on a stream

When a background job fails, the log file ends with a single `result` event whose `status="error"` and `error` carries the message. Consumers should treat the absence of a terminal `result` as "process killed before reporting" and fall back to checking `bcmr status <job_id>` — the status command reads the on-disk JobInfo and the log tail.

## What is *not* yet specified

- **Schema versioning.** No `version` field today. Treat the schema as v0; assume best-effort backwards compat within a minor release.
- **`phase` events.** A consumer wanting to know "scan phase finished, transfer phase started" today infers it from `scanning: true → false` between two `progress` events. A future explicit `{"type":"phase","name":"scan_complete"}` would beat the heuristic.
- **Reflink / dedup / compress counters.** A future PR will add `reflink_count`, `dedup_count`, and `compress_ratio` to the `result` event. Treat absent fields as zero/unknown.
- **Operation-string enum.** `operation` is freeform today. Don't pattern-match; if you need to know the phase, use `scanning` or upcoming `phase` events.

## See also

- `bcmr status [job_id]` — query status and tail logs of a background job.
- `~/.local/share/bcmr/jobs/<job_id>.jsonl` — per-job log files (auto-cleaned after 7 days).
