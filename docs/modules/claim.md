Verifies the caller owns the latest open assignment before transitioning.
Two agents racing the same task produce exactly one winner and one
conflict error — this is the atomicity the whole design rests on.

## Why there are three checks and not just the guard

The guarded `UPDATE` alone would be *safe* — it cannot corrupt anything — but it
would be uninformative. It only knows the task was not `assigned`; it cannot
distinguish "nobody assigned this to you" from "you already claimed it" from
"someone else holds it". Those want three different reactions from a calling
agent, so they are separated before the transition into `no_open_assignment`,
`already_claimed`, and a `not_owner` error. All three read the assignment row
inside the same `IMMEDIATE` transaction as the `UPDATE`, which is what keeps them
from being the read-then-write banned elsewhere in this crate.

The guard behind them stays anyway, and its error code says what it is:
`state_changed_under_us`. Reaching it means the assignment row and the task state
disagreed at the moment of the write. No current command sequence should produce
that; if it ever fires, the bug is in whatever moved the task without closing its
assignment, not here.

## Ownership means the *latest open* assignment

`db::current_assignment` filters to `completed_at IS NULL`. That is a different
question from `store::latest_agent`, which returns the most recent assignment row
whether or not it is closed. The two are not interchangeable, and confusing them
is how a released task appears to still belong to the agent that dropped it —
which is exactly what `qp list` shows in its agent column, by design.
`depends_uses_latest_open_assignment_not_latest_by_id` pins the distinction on
the command most likely to get it wrong.

Claiming also stamps `claimed_at` on the assignment row, and that timestamp is
load-bearing rather than decorative: `complete` refuses a task whose assignment
was never claimed, so going straight from `assign` to `complete` fails instead of
quietly working.
