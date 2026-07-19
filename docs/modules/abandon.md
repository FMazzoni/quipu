Ownership-checked: an agent may only release its own assignment. The guard
accepts `assigned` or `running` and returns the task to `pending` rather than
guessing whether its deps still hold; `refresh_ready` promotes it when they do.
Compare `reclaim`.

## Routing through `pending`

Landing straight back in `ready` would mean writing the readiness rule a second
time. Between assignment and release a dependency may have been added, or a
prerequisite reopened, so the state a released task belongs in is a question only
`refresh_ready` should answer. The unconditional round trip through `pending`
costs one extra `UPDATE` and guarantees the release path cannot disagree with the
promotion path. Both outcomes are pinned:
`abandon_returns_to_ready_when_no_unresolved_deps` and
`abandon_returns_to_pending_when_unresolved_dep_exists`.

The event payload therefore cannot be written from a constant. The resulting
state is read back after `refresh_ready` — an auxiliary read inside the
`IMMEDIATE` transaction, for event quality rather than control flow, which is the
permitted form of the otherwise-banned read-then-write.

## Split from `reclaim`

Both land the task in the same place; the difference is who is allowed to run
them. `abandon` takes `--as` and is refused with `not_owner` when the caller is
not the assignee. `reclaim` takes no agent id and checks nothing. Separate
commands mean an agent cannot release someone else's work by reaching for the
convenient verb, and an orchestrator cleaning up after a dead process does not
need credentials it never had.

## Closing the assignment

The close is guarded too, targeted at the specific row id this agent held and
requiring `completed_at IS NULL`. Because `with_tx` holds the write lock, no
concurrent process can have closed it first, so a count other than one means the
open-assignment invariant was already broken before this command ran; it reports
`already_closed` and the whole transaction rolls back rather than releasing the
task with its assignment left open. That is the shape of corruption `assign`'s
`stale_open_assignment` guard catches downstream.
