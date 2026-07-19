Ownership-checked: an agent may only release its own claim. Returns the
task to `pending` rather than guessing whether its deps still hold;
`refresh_ready` promotes it when they do. Compare `reclaim`.

## Why `pending` and not straight back to `ready`

Because deciding otherwise would mean writing the readiness rule a second time.
Between claim and release, a dependency may have been added, or a prerequisite
may have been reopened — so the state a released task belongs in is a question
only `refresh_ready` should answer. Routing through `pending` unconditionally
costs one extra `UPDATE` and buys a guarantee that the release path can never
disagree with the promotion path. Both outcomes are pinned:
`abandon_returns_to_ready_when_no_unresolved_deps` and
`abandon_returns_to_pending_when_unresolved_dep_exists`.

The consequence is that the event payload cannot be written from a constant. The
resulting state is read back after `refresh_ready` — an auxiliary read inside the
`IMMEDIATE` transaction, for event quality rather than control flow, which is the
permitted form of the otherwise-banned read-then-write.

## The difference from `reclaim` is who is allowed, not what happens

Both land in the same place. `abandon` is the agent saying "I am putting this
down" and is refused with `not_owner` if the caller is not the assignee;
`reclaim` is the orchestrator saying "that agent is gone" and asks nobody's
permission. Keeping them as separate commands means an agent cannot accidentally
release someone else's work by reaching for the convenient verb, and an
orchestrator cleaning up after a dead process does not need credentials it never
had.

Closing the assignment is guarded too, targeted at the specific row this agent
held and requiring `completed_at IS NULL`. The write lock means no concurrent
process can have closed it first, so a count other than one indicates the
open-assignment invariant was already broken before this command ran. The whole
transaction rolls back rather than releasing the task with its assignment left
open — which is the exact shape of corruption `assign`'s `stale_open_assignment`
guard exists to catch downstream.
