The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<id>`, `<duration>` and friends as unknown HTML tags and silently deletes them,
losing every argument placeholder (QP-96).

```text
Modes:
  default            board payload: { tasks, events, deps }
  --ticket <id>      full detail for one ticket (parents, children, uncapped events)
  --all-tickets      JSON array of the same per-ticket detail, one per ticket in scope

Scope filters:
  --since <duration> filter events: 24h, 7d, or RFC3339 date
  --wave  <task-id>  scope to the dep subtree of the given task

Output:
  stdout by default, or --output <path> to write to a file.
```

## Event cap

The default board payload caps its event list at 200. The per-ticket modes
(`--ticket`, `--all-tickets`) do not cap at all. The split follows what each mode
is for: a board is a dashboard snapshot, where an unbounded event list is
overhead nobody reads, while a per-ticket view is the forensic one — truncating a
single ticket's history would drop exactly the evidence someone opened it to
find.

The cap keeps the **oldest** 200 events in scope, not the newest.
`store::events` orders `e.id ASC` and `collect_json` breaks out of the loop at
200, so on a store with more than 200 events in scope the board's `events` array
ends before the present. Narrow with `--since`, or use `--ticket`, when recency
or completeness matters.

Scope filters compose but work on different keys. `--wave` resolves to a set of
task rowids, while events carry only a display id, so scoping events to a subtree
needs a display-id-to-rowid lookup built for the purpose. A dep edge is in scope
only when *both* of its endpoints are — a half-visible edge would render as a
dependency on nothing.

## Rendering

Markdown and HTML rendering used to live in this binary. It now lives in the
`skills/report-render/` skill — see that skill for the section structure (state
snapshot, in-flight, timeline, friction log, open bugs, shipped) that an agent
reconstructs from this JSON.
