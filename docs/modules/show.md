The single-ticket view: everything about one task on one screen, where `list` and
`tree` show a little about many.

That framing is the whole design rule, and it decides the arguments. `show`
resolves exactly one reference and never filters, so it has no query language to
learn — if you know the id, this is the command. Its human mode is laid out to be
read top-down by a person picking up a ticket (identity, then metadata, then the
description, then what has happened to it); its JSON mode is deliberately a
superset of that same record, so an agent and a human are looking at the same
facts rather than two views that can disagree.

The header line is unlabelled positional columns, and the rule that keeps it
readable is that it carries **exactly `list`'s columns in `list`'s order** — id,
state, agent, tags — so a reader moving between the two commands can never
misread one for the other. Everything else, `tier` included, goes in the
labelled block below, where a field is named rather than located.

That rule was learned twice. The header originally omitted the agent entirely,
which left the unlabelled `tier` column sitting exactly where `list` puts AGENT;
a reader cross-checking the two commands saw tier's `-` as "no agent" and
concluded `list` was wrong (QP-154). Inserting the agent fixed the reported
symptom but left tier adjacent to it — still unlabelled, still a column `list`
does not have — so QP-156 moved tier down into the labelled block instead of
shifting the ambiguity one position over. If a future field wants to be in the
header, the question to ask is whether `list` carries it; if not, label it.

The agent appears in both places, and both render the same value: the *latest*
assignee from `store::latest_agent`, which is what `list` and `wave` show too.
That name outlives the assignment — after an `abandon` nobody holds the task but
the last assignee is still named — so it is not evidence of an open claim;
ownership questions go to `db::current_assignment`. The redundancy is the point:
one field rendered one way in one command is worth a duplicated word.

`--json` is unaffected by any of this. It carries `tier` as a top-level field
and always has; the column layout is a human-readable rendering choice, and
scripts are expected to read the JSON.

The event tail is capped in human mode only, and the cap is a readability limit
rather than a statement about the data. `HUMAN_EVENT_TAIL` is an arbitrary
number — it exists so a ticket with hundreds of events still fits on a screen,
and nothing else depends on it. An earlier version of this document claimed the
cap encoded a distinction between "context" and "audit trail"; it did not, and
that rationale was invented after the fact (QP-152). What the cap does owe the
reader is a signal: when it drops events the human view says how many and points
at `qp timeline <id>`, because a silently shortened list is what made an
arbitrary number look like a promise.

`--json` is uncapped. Machine output is complete or it is misleading — a
consumer handed ten events with no marker cannot tell a quiet ticket from a
truncated one — so `show --json` and `report --ticket` return the same full
history.

Ordering is worth knowing because the two modes reverse each other. The query
takes the newest events, then human mode prints them oldest-first so a reader
scans downward in the order things happened. JSON emits `recent_events`
oldest-first for the same reason, alongside a separate `last_event` field so a
consumer wanting only the latest does not have to know which end of the array to
read.

`blocked_by` comes from the shared unresolved-dependency query in `store.rs`, not
from a local `SELECT`, so what `show` calls blocked is the same thing `wave` and
the readiness rule call blocked. Sorting is numeric rather than lexicographic —
`show_blocked_by_sorts_numerically_not_lexically` exists because display ids sort
by string in a way no reader expects once a store passes ten tasks.

`wrap_text` lives here rather than in `store.rs` because it touches no database;
`store.rs` is for queries, and a text helper is not one.
