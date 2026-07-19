A filter over the same event-tail query `timeline` uses.

There is no decisions table. A decision is an ordinary row in the append-only
`event` log with `kind = 'decision'`, and this command is one `EventFilter` over
`store::events`. That is the durable point: decisions inherit the log's
guarantees for free — they are written in the same transaction as the work they
describe, they are ordered by the same gap-free `event.id`, and they cannot go
missing while their task's state change survives.

`--auto-only` selects entries whose payload carries `auto: true`, the flag
subagents set when logging friction notes at the end of a slice. The split
matters because the two populations answer different questions: everything is the
project's reasoning record, while the auto subset is the machine-generated retro
feed. Both render identically and share a row shape, deliberately, so a consumer
can switch between them without a second parser
(`decisions_json_and_auto_only_json_share_row_shape`).

`--since` is an **exclusive** lower bound on `event.id`: `--since 730` returns
events starting at 731. It is exclusive because it compiles to the same
`e.id > ?` clause in `store::events` that backs `timeline --since` — the two
commands cannot drift apart on this, which is the point of routing both through
one `EventFilter` instead of growing a parallel query here. The flag exists
because a wave needs "what was decided since I started", and the alias could not
express that without it; coordinators previously had to reach past `decisions`
into `timeline --kind decision --since` to get it.

Omitting the flag leaves `since_id` as `None`, not `Some(0)`. Today those give
the same answer because event ids start at 1, but "no lower bound" is the honest
statement of intent and does not depend on that coincidence.

Deliberately absent: `--tag`. Every flag added to a filter alias makes it less of
an alias, and `--since` alone covers the wave-scoping case that motivated the
change. Callers needing richer scoping should use `timeline`, which is the
general command this one is a shorthand for.
