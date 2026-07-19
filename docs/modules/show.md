The single-ticket view: everything about one task on one screen, where `list` and
`tree` show a little about many.

That framing is the whole design rule, and it decides the arguments. `show`
resolves exactly one reference and never filters, so it has no query language to
learn — if you know the id, this is the command. Its human mode is laid out to be
read top-down by a person picking up a ticket (identity, then metadata, then the
description, then what has happened to it); its JSON mode is deliberately a
superset of that same record, so an agent and a human are looking at the same
facts rather than two views that can disagree.

The event tail is capped rather than complete. `show` answers "what is going on
with this ticket", and the recent events are context for the current state, not
an audit trail — the uncapped history is `qp timeline --task` and
`qp report --ticket`. If you find yourself wanting the cap raised, the question
being asked has probably become a forensic one and belongs to those commands.

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
