---
name: report-render
description: Render a Markdown or HTML report from qp's `report --json` payload. Use when asked for a wave/project status report, a per-ticket writeup, or a dashboard-style snapshot.
---

## When to use

`qp report` only emits JSON — no Markdown/HTML renderer ships in the binary
(see `decisions/` in the vault, or QP-73, for why). This skill is the
rendering half: fetch the JSON, then produce prose/markup from it yourself.

## Fetching the payload

```bash
qp report --json                        # board snapshot: tasks + events + deps
qp report --json --since 24h            # scope events to a window (Nh, Nd, or RFC3339 date)
qp report --json --wave QP-7            # scope to QP-7's transitive dep subtree
qp report --json --ticket QP-12         # full detail for one ticket
qp report --json --all-tickets          # array of per-ticket detail, scoped by --since/--wave
qp report --json --output snapshot.json # write to a file instead of stdout
```

`--since` and `--wave` compose with the default and `--all-tickets` modes.
`--ticket` and `--all-tickets` are mutually exclusive with each other.

## Payload shapes

### Default (board snapshot)

```json
{
  "tasks": [
    {
      "id": 1, "display_id": "QP-1", "title": "...", "state": "running",
      "tier": "wave-7", "agent": "claude-code:wt-a",
      "description": "...",            // omitted if null
      "tags": ["wave:7", "kind:bug"],
      "blocked_by": ["QP-3"],           // non-done/cancelled deps, display_ids
      "last_event": {"kind": "state_change", "ts": "...", "payload": {...}}
    }
  ],
  "events": [
    {"id": 42, "task": "QP-1", "ts": "...", "kind": "decision", "agent_id": "...", "payload": {...}}
  ],                                    // scoped by --since, capped at 200, ascending by id
  "deps": [{"from": "QP-2", "to": "QP-1"}]  // QP-2 depends on QP-1
}
```

### `--ticket QP-N` (single object, not wrapped in a list)

```json
{
  "display_id": "QP-12", "title": "...", "state": "done", "tier": "wave-7",
  "agent": "claude-code:wt-b", "description": "...", "created_at": "...",
  "tags": ["wave:7", "commit:abcd123"],
  "parents": [{"display_id": "QP-9", "title": "...", "state": "done"}],   // this depends on
  "children": [{"display_id": "QP-15", "title": "...", "state": "ready"}], // depend on this
  "events": [{"ts": "...", "kind": "note", "agent_id": "...", "payload": {...}}]
  // ^ full chronological (ascending) history, uncapped — unlike the board's events list
}
```

### `--all-tickets` (array of the object above, one per ticket in scope)

```json
[ { "display_id": "QP-1", ... }, { "display_id": "QP-2", ... } ]
```

## Rendering a full report (the section structure)

When asked for a "status report" or "wave report", reconstruct these
sections in order from the default-mode payload (or `--wave`-scoped payload
for a single wave):

1. **Header** — title, a generation timestamp (use current time; the
   payload doesn't carry one now — see Note below), and the scope you
   queried with (`--since`/`--wave` values, if any).
2. **State snapshot** — count `tasks` by `state`. Show all of `pending`,
   `ready`, `assigned`, `running`, `done`, `cancelled` (zero-fill missing
   ones).
3. **In flight** — tasks with `state` in `ready`/`assigned`/`running`, plus
   `pending` tasks whose `blocked_by` is non-empty.
4. **Recent timeline** — walk `events` newest-first (reverse the array).
   Note if it's exactly 200 long, since that's the payload's cap — say so
   and suggest narrowing with `--since`.
5. **Friction log** — filter `events` to `kind == "decision"` where
   `payload.auto == true`. Render `payload.text` per entry.
6. **Open bugs** — tasks with `"kind:bug"` in `tags` and `state` not in
   `done`/`cancelled`.
7. **Recently shipped** — tasks with `state == "done"`; pull the commit sha
   from a `commit:<sha>` tag if present.

For a **per-ticket** report, render: header (display_id + title), state/
tier/agent/created_at, tags (pull out `commit:`, `plan:`, `critique:`,
`harness:` prefixes as labeled fields, list the rest), description, a
"Related tickets" section from `parents`/`children`, and a full timeline
table from `events` (it's already uncapped and chronological).

## Note: what the JSON payload doesn't carry

- No `generated_at` timestamp field — stamp it yourself when you render.
- No pre-computed "friction"/"open bugs"/"shipped" sub-lists — derive them
  from `tasks`/`events`/`tags` per the recipe above. This is a few lines of
  filtering, not a data problem.

## Also consumed by

`board/` (a Svelte dashboard) reads exactly this payload —
`qp report --json > board/public/data.json` — as a second, non-agent
consumer. If you change what you render here, it's still the same payload
`board/src/lib/data.ts` parses; don't expect to change the JSON shape from
this skill alone.

## Shipping this skill

`qp install-skills` copies everything under `skills/` into the project's
`.claude/skills/` (or equivalent) — this skill ships automatically, no
separate registration step.
