This is the single-ticket view: everything about one task on one screen, where
`list` and `tree` show a little about many.

## Scope

`show` resolves exactly one reference and never filters, so it has no query
language. Human mode is ordered to be read top-down by a person picking up a
ticket — identity, then metadata, then the description, then event history. The
`--json` object is a superset of that record, so an agent and a human see the
same facts rather than two views that can disagree.

## Header columns

The header line is unlabelled positional columns carrying **exactly `list`'s
columns in `list`'s order** — id, state, agent, tags. Everything else, `tier`
included, goes in the labelled block below, where a field is named rather than
located.

That rule was learned twice. The header originally omitted the agent, which left
the unlabelled `tier` column sitting exactly where `list` puts AGENT; a reader
cross-checking the two commands read tier's `-` as "no agent" and concluded
`list` was wrong (QP-154). Inserting the agent fixed the reported symptom but
left tier adjacent to it — still unlabelled, still a column `list` does not have
— so QP-156 moved tier into the labelled block rather than shifting the ambiguity
one position over. If a future field wants to be in the header, the question is
whether `list` carries it; if not, label it.

`--json` is unaffected by the layout: it carries `tier` as a top-level field, and
scripts are expected to read the JSON.

## Agent field

The agent appears in the header and again in the labelled block, and both render
`store::latest_agent` — the *latest* assignee, which is what `list` and `wave`
show too. That name outlives the assignment: after an `abandon` nobody holds the
task but the last assignee is still named, so it is not evidence of an open
claim. Ownership questions go to `db::current_assignment`.

## Event tail

The event query is unbounded; human mode truncates the result to
`HUMAN_EVENT_TAIL`. The cap is a readability limit, not a statement about the
data — an arbitrary number that exists so a ticket with hundreds of events still
fits on a screen. An earlier version of this document claimed the cap encoded a
distinction between "context" and "audit trail"; it did not, and that rationale
was invented after the fact (QP-152). What the cap does owe the reader is a
signal: when it drops events, human mode prints how many and points at
`qp timeline <id>`.

`--json` is uncapped. Machine output is complete or it is misleading — a consumer
handed ten events with no marker cannot tell a quiet ticket from a truncated one
— so `show --json` and `report --ticket` return the same full history.

## Event ordering

The query returns events newest-first (`ORDER BY id DESC`), which is what lets
the tail keep the most recent ones. Both output modes then reverse that: human
mode prints oldest-first so a reader scans downward in the order things happened,
and JSON emits `recent_events` oldest-first for the same reason. JSON also
carries a separate `last_event` field, so a consumer wanting only the latest does
not have to know which end of the array to read.

## Blocked-by

`blocked_by` comes from `store::unresolved_blockers_by_task`, not from a local
`SELECT`, so what `show` calls blocked is what `wave` and the readiness rule call
blocked. Sorting is numeric rather than lexicographic —
`show_blocked_by_sorts_numerically_not_lexically` exists because display ids sort
by string in a way no reader expects once a store passes ten tasks.

## wrap_text

`wrap_text` lives here rather than in `store.rs` because it touches no database;
`store.rs` is for queries, and a text helper is not one.
