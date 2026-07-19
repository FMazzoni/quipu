Records decisions and artifacts as events on the way through.

The only success edge. Everything else that leaves `running` — `abandon`,
`reclaim`, `block`, `cancel` — either recycles the task or kills it, so this is
the one place where finishing work is distinguished from stopping work, and the
assignment row is closed with `outcome = 'success'` to say so.

Three preconditions, and the middle one is the interesting one: the caller must
hold an open assignment, that assignment must already be **claimed**, and the
caller must be its agent. Requiring `claimed_at` means `assign` → `complete` with
no `claim` in between is rejected rather than quietly succeeding — closing a real
hole in agent workflows, where a subagent that never actually started could
otherwise mark its own ticket done.

`--decision` and `--artifact` are written as separate events *before* the
`state_change`, so a timeline reads in the order the work happened: reasoning
first, then the transition. Both are repeatable and each value becomes its own
row. A decision is a unit of provenance, not a text field — that is what lets
`qp decisions` filter across the whole store without parsing prose.

`refresh_ready` runs last, inside the same transaction. Completing a task is the
most common way a dependency gets resolved, so anything waiting on this one is
promoted before the write lock is released, and no reader can observe a `done`
task whose dependents are still `pending` because of it
(`complete_marks_done_records_decisions_unblocks_deps`).
