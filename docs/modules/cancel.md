`cancelled` counts as resolved for dependency purposes, so cancelling a
blocker promotes whatever it was blocking.

## Cancelled as a resolved dependency

`refresh_ready` in `db.rs` treats a dependency as unresolved only while its state
is `NOT IN ('done','cancelled')`, so the DAG asks "is this prerequisite still in
flight", not "did it succeed". Abandoning a line of work therefore unblocks its
dependents rather than stranding them. The same predicate is what makes
`qp wait --cohort-done` return on a cohort whose tasks were cancelled rather than
finished (`wait_cohort_done_treats_cancelled_as_drained`). Killing dependents
instead is a policy for the calling skill; the substrate does not encode it.

## Guard and ownership

The guard is `state NOT IN ('done','cancelled')` rather than a single expected
state, because this edge starts from anywhere in flight. A second cancel matches
no row and reports `already_terminal`, exit 2.

There is no ownership check: cancelling is a decision about whether work should
exist at all, not about who holds it. Any open assignment for the task is closed
in the same transaction with `outcome = 'cancelled'`, and `refresh_ready` runs
last so dependents are promoted before the write lock is released.
