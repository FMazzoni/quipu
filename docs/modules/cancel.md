`cancelled` counts as resolved for dependency purposes, so cancelling a
blocker promotes whatever it was blocking.

That equivalence is the design decision worth knowing, and it is not obvious. A
dependency asks "is this prerequisite still in flight", not "did it succeed" —
so `done` and `cancelled` are interchangeable to the DAG, and abandoning a line
of work unblocks its dependents rather than stranding them forever. The same
equivalence is what makes `qp wait --cohort-done` return on a cohort whose tasks
were cancelled instead of finished
(`wait_cohort_done_treats_cancelled_as_drained`). If a cancelled prerequisite
should instead kill its dependents, that is a policy for the calling skill to
apply — the substrate does not encode it.

The guard here is `NOT IN ('done','cancelled')` rather than a single expected
state, because this edge starts from anywhere in flight. It is therefore the one
transition that is *not* idempotent-looking-but-safe: a second cancel reports
`already_terminal` and exits 2, which is the honest answer.

No ownership check. Cancelling is an orchestrator decision about whether work
should exist at all, so it does not ask whose desk the task is on — but any open
assignment is closed in the same transaction, with `COALESCE` so an
already-stamped `completed_at` is not overwritten. `refresh_ready` then runs for
the dependents.
