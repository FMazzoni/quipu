Two agents racing the same task produce exactly one winner and one conflict
error — this is the atomicity the rest of the design rests on.

## Three pre-checks and the guard

The guarded `UPDATE` alone would be safe but uninformative: it knows only that
the task was not `assigned`, and cannot distinguish "nobody assigned this to
you" from "you already claimed it" from "someone else holds it". Those want
three different reactions from a calling agent, so they are separated before the
transition, in this order — `no_open_assignment`, `already_claimed`, then
`not_owner`. All three read the assignment row inside the same `IMMEDIATE`
transaction as the `UPDATE`, which is what keeps them from being the
read-then-write banned elsewhere in this crate.

The guard behind them stays, with the code `state_changed_under_us`. Reaching it
means the assignment row and the task state disagreed at the moment of the
write. No command sequence produces that; if it fires, the bug is in whatever
moved the task without closing its assignment.

## Latest open vs latest assignment

`db::current_assignment` filters to `completed_at IS NULL` and takes the highest
`id`. `store::latest_agent` takes the highest `id` regardless of
`completed_at`, so it names the most recent assignee whether or not that
assignment is still open — which is what `qp list` shows in its agent column, by
design. The two are not interchangeable, and confusing them is how a released
task appears to still belong to the agent that dropped it.
`depends_uses_latest_open_assignment_not_latest_by_id` pins the distinction.

## `claimed_at`

Claiming stamps `claimed_at` on the assignment row. `complete` rejects an
assignment whose `claimed_at` is null with `not_claimed`, so `assign` straight to
`complete` fails rather than quietly working.
