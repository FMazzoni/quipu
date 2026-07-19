This is the only success edge. The other commands that leave `running` —
`abandon`, `reclaim`, `block`, `cancel` — return the task to `pending` or make it
terminal, so this is the one place that distinguishes finishing work from
stopping it. The assignment row is closed with `outcome = 'success'` to say so.

## Preconditions

Three, checked in this order: the caller must hold an open assignment
(`no_open_assignment`), that assignment must already be claimed
(`not_claimed`), and the caller must be its agent (`not_owner`). The guarded
`UPDATE` from `running` to `done` then reports `not_running`.

The `claimed_at` check is an error-quality guard, not a data-integrity one.
`claim` is the only writer that puts a task into `running` and stamps
`claimed_at` in the same transaction, so `running` implies claimed and the state
guard would reject an unclaimed task on its own. What the check buys is the
error: because it runs first, `assign` → `complete` with no `claim` in between
fails as `not_claimed` rather than `not_running`. Removing it breaks no
invariant and degrades the diagnostic.

## Event order

`--decision` and `--artifact` are written as separate events before the
`state_change`, so a timeline reads in the order the work happened. Both flags
are repeatable and each value becomes its own event row, which is what lets
`qp decisions` filter across the store without parsing prose.

## Dependency promotion

`refresh_ready` runs last, inside the same transaction. Completing a task is the
most common way a dependency gets resolved, so anything waiting on this one is
promoted before the write lock is released and no reader observes a `done` task
whose dependents are still `pending`
(`complete_marks_done_records_decisions_unblocks_deps`).
