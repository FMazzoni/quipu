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

## `--output` never crosses the socket

The path names a file on the *caller's* disk, so it is `#[serde(skip)]` and the
write happens client-side: `report` calls `route::fetch` for the payload and
does its own output, rather than going through `route::dispatch`. Served, the
snapshot would land on the server's filesystem and the caller would get nothing
— the same test the wire contract applies to `export` and `init`, which is to
ask whose disk the path is on.

`--since` is the other half of the same question and answers differently: it is
a *value*, not a location, so it crosses the wire and is parsed on the server.
`24h` therefore resolves against the server's clock, which is correct because it
is bounding that store's own `event.ts` values.

## Staleness on the board payload

Each entry in `tasks` carries `stale` and `hold`, spelled exactly as
`wave --json` and `list --json` spell them: a boolean beside the row, and the
hold object (`agent`, `host`, `last_contact`, `lease_seconds`, `expires_at`,
`stale`) under `hold`, or `null` when nothing holds the task. A renderer written
against one of the three views reads the others unchanged.

This exists because the dashboard reads this payload and nothing else. Without
it a ticket stranded by a machine that died renders as a `running` row
indistinguishable from work in progress — which is the whole failure the lease
was built to make visible.

The holds come from `lease::holds_by_task`, one statement for the page rather
than one per row, and it embeds the same `LIVE_HOLD` predicate `reclaim` breaks
holds with, so a board cannot call something stale that `reclaim` would refuse
to take. A hold that stated no expiry is never stale.

The per-ticket modes (`--ticket`, `--all-tickets`) deliberately do not carry it.
They are the forensic view of one ticket's history, `qp show` already answers
the liveness question for a single ticket with the same `hold` object, and
adding it to `--all-tickets` would put a per-row query where the board payload
has one.
