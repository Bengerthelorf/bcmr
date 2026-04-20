# Commands spec

This directory is the **canonical CLI reference** for bcmr. Each top-level
subcommand is one markdown file (`copy.md`, `move.md`, …). The site reads
every `*.md` here, validates the frontmatter, and renders the page at
`/bcmr/commands` as a searchable reference with a sidebar, flags table,
and terminal example.

Keep files thin: the frontmatter *is* the data. The markdown body is
optional long-form prose that shows in the detail panel when a reader
picks a command.

## Frontmatter fields

| field      | type       | required | what                                                                |
| ---------- | ---------- | -------- | ------------------------------------------------------------------- |
| `cmd`      | string     | yes      | full command as typed, e.g. `bcmr copy`                             |
| `group`    | string     | yes      | sidebar group; free-form — `core` / `shell` / `system` etc.         |
| `sig`      | string     |          | args signature after the command, e.g. `<sources>... <destination>` |
| `desc`     | string     | yes      | one-liner shown in sidebar                                          |
| `tags`     | string[]   |          | status badges — `stable`, `beta`, `core`                            |
| `order`    | number     |          | sort within group; defaults to file order                           |
| `related`  | string[]   |          | `cmd` names to link in the "related" footer                         |
| `flags`    | object[]   |          | each `{ f, t, d, x }` — flag / type / default / description         |
| `example`  | string[]   |          | stdout lines for the demo terminal; last line rendered as "ok"      |

Minimal file:

```markdown
---
cmd: bcmr doctor
group: system
desc: probe environment for fs capabilities, ssh agent, reflink support
tags: [stable]
---
```

Full file with flags + example:

```markdown
---
cmd: bcmr copy
group: core
sig: <sources>... <destination>
desc: copy files or directories with blake3 verification; src and dst may be local or ssh
tags: [stable, core]
order: 1
related: [bcmr move, bcmr check, bcmr remove]
flags:
  - { f: "-r, --recursive",  t: bool,                     d: "false",      x: "recursively copy directories" }
  - { f: "-V, --verify",     t: bool,                     d: "false",      x: "recompute blake3 after copy" }
  - { f: "--reflink",        t: "auto|force|disable",     d: "auto",       x: "copy-on-write mode" }
  - { f: "-j, --jobs <N>",   t: int,                      d: "min(CPU,8)", x: "max concurrent file copies" }
example:
  - "scanning source …… 2,481 files · 14.2 gb"
  - "transfer …… 412 mb/s"
  - "complete · all blake3 verified"
---

Optional long-form markdown here. Links, code, lists — anything a reader
might want after they've picked this command.
```

## Authoring conventions

- **Group names** are lowercase, one word where possible (`core`, `shell`,
  `system`, `integrity`, `transfer`, `inspect`). Groups sort in the order
  they first appear in the collection — use `order` on commands for
  in-group ordering.
- **Flag column `t`** is free-form. `bool`, `int`, `str`, `path`, or enum
  pipes like `auto|force|disable` all render the same way.
- **Related** items must match another command's `cmd` exactly. The site
  resolves them to links at render time.
- **Example** lines are literal shell output — no syntax highlighting, one
  line per array entry. Keep them short enough to not wrap in the detail
  panel (roughly under 80 chars).

## Non-goals

- This is not a place for tutorials. Use `/guide/*.md` for prose how-tos.
- Don't duplicate flag docs from the Rust source — keep `x` descriptions
  short; deep behaviour belongs in guide docs, linked via `related`.
