Records decisions and artifacts as events on the way through.

The only success edge. Everything else that leaves `running` — `abandon`,
`reclaim`, `block`, `cancel` — either recycles the task or kills it, so this is
the one place where finishing work is distinguished from stopping work, and the
assignment row is closed with `outcome = 'success'` to say so.

Three preconditions: the caller must hold an open assignment, that assignment
must already be **claimed**, and the caller must be its agent.

The `claimed_at` precondition is an error-quality guard, not a data-integrity
one, and that is worth stating because it otherwise reads as dead code. `claim`
is the only writer that puts a task into `running`, and it stamps `claimed_at` in
the same transaction — so `running` implies claimed, and the state guard on the
transition would reject an unclaimed task on its own. What the check buys is
which error comes back: it runs first, so `assign` → `complete` with no `claim`
in between fails as `not_claimed` ("QP-1 not claimed") rather than the vaguer
`not_running`. Assigned but never claimed is the common mistake, and naming it is
the entire value here. Removing the check breaks no invariant; it silently
degrades the diagnostic.

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
