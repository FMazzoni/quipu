The pull-model counterpart to `assign`: N independent machines each take their
own next ticket, with no coordinator handing them out.

## Why selection and assignment are one statement

The sequence this replaces is `qp list --state ready` followed by
`qp assign <id>` — a read-then-write across two processes. The guarded `UPDATE`
in `assign.rs` means the loser of that race is refused rather than corrupting
anything, but being refused is not the same as being served: the loser has to
re-list and re-choose, and nothing in the tree or in any skill does that.

So selection and assignment are one statement, because any gap between them is a
window in which two machines choose the same ticket — and the loser of a guarded
`UPDATE` has no way to retry that the caller does not have to write.

A `SELECT` followed by an `UPDATE` inside the same transaction would be correct
under `IMMEDIATE` — the write lock is already held — and is still not what ships
here. It reintroduces the shape the guarded-transition rule exists to ban, one
layer down, where the next reader has to re-derive that the lock is what makes it
safe. `UPDATE ... WHERE id = (SELECT ... LIMIT 1) AND state = 'ready'` needs no
such argument: one statement, one guard, one `changes()` check.

`RETURNING` is what makes that possible. It hands back the row the `UPDATE`
matched, so the follow-up writes — the `assignment` row and the `state_change`
event — key off the id the statement itself chose, never off a second lookup.

## Readiness

The candidate subquery repeats `db::refresh_ready`'s readiness predicate and
excludes `db::FROZEN` rather than trusting `task.state` alone. `state = 'ready'`
is a materialised projection of those two, maintained by `refresh_ready_logged`
and `demote_frozen` at the edges that can change it; repeating the definition
here means a store whose projection has drifted — an older store, a bug in a new
edge — cannot hand a taker a ticket whose prerequisites are unresolved. It is
the same notion of readiness, written once more, not a second one.

## Order

Oldest first, `ORDER BY t.id ASC`: quipu ids are monotonic, so this is arrival
order, which is what a work queue means by "next". It also makes the result
reproducible — a fixed store and a fixed filter always yield the same ticket —
which is what lets the concurrency tests assert *which* tickets were taken
rather than merely how many. Priority ordering is deliberately absent; `tier` is
a shape, not a rank, and inventing a rank in the binary would be the CLI taking a
position on a scheduling policy that belongs to a skill.

## Filters

`--tag` mirrors `qp list`: repeatable, glob-matched, all must match. `qp list`
has no `--tier` flag, so neither does this — the two filter vocabularies are
meant to stay the same one, and adding a flag here that `list` lacks means an
operator cannot preview what a taker would take.

## The empty cohort is not an error

"Nothing to take" exits **0**, with `taken` explicitly null under `--json` and
the line `nothing to take` otherwise. It is not exit 2 and not a dedicated code.

Exit 2 is documented in `docs/architecture.md` as "the store said no — retrying
may succeed", which is precisely the branch a polling machine retries on; a
drained queue answering 2 turns every idle taker into a hot loop. Nor did
anything fail: the command was asked to look, it looked, and the store is in a
valid state. A dedicated non-zero code was rejected for the same reason `wait`'s
exit 4 does not generalise — an empty cohort there means a typo'd tag, whereas
here it is the ordinary steady state of a finished wave, and under `set -e` the
most common outcome would abort the loop that polls for it.

The distinction therefore lives in the payload, where a consumer already looks:

```text
$ qp take --as w1 --json          # took one
{"taken":{"display_id":"QP-7","title":"...","state":"assigned"},"agent_id":"w1"}
$ qp take --as w1 --json          # queue drained
{"taken":null,"agent_id":"w1"}
```

A shell poller branches on `jq -e '.taken != null'`; the exit code stays
reserved for things that actually went wrong.

## Boundaries

`take` stops at `assigned`, exactly where `assign` stops. It does not claim, so
the `assigned` → `running` edge — and with it the lease a claim takes — stays in
`claim.rs`. A machine that has taken a ticket still runs `qp claim`. Folding the
two together would collapse two distinct edges into one, and a caller that wants
the first without the second would have no way to ask.

Which ticket a wave *should* hand out next is not modelled here either. This
command answers "the oldest ready one matching this filter"; anything cleverer
is a pattern, and patterns live in `skills/`.
