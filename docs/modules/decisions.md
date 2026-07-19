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

Note what the filter does *not* set: `since_id` is `None`, not `Some(0)`. Today
those give the same answer because event ids start at 1, but "no lower bound" is
the honest statement of intent and does not depend on that coincidence.
