This is the force-release path: same destination as `abandon`, no ownership
check, for when an agent has died and cannot release its own assignment.

## Separate command, not an `abandon` flag

An orchestrator recovering from a crashed subagent has no way to prove it is that
agent and should not have to impersonate one. A flag would put that power one
typo away from every agent that meant to release its own work; a separate verb
has to be reached for deliberately. `reclaim` takes no `--as`
(`reclaim_force_releases_without_agent_id`).

Everything downstream matches `abandon`: the guarded `UPDATE` accepts `assigned`
or `running` and sets `pending`, `refresh_ready` decides whether it comes
straight back to `ready`, and the resulting state is read back for the event
payload rather than assumed
(`reclaim_returns_to_pending_when_unresolved_dep_exists`).

## Two asymmetries against `abandon`

The assignment close targets *every* open row for the task (`WHERE task_id = ?
AND completed_at IS NULL`) rather than one known row id. This is a cleanup path:
if an earlier bug left two open assignments, `reclaim` should mop them up rather
than trip over them. It therefore checks the count for zero rather than for
exactly one — closing several is a success, closing none reports
`no_open_assignment`. The guarded `UPDATE` runs first, so a task that is neither
`assigned` nor `running` reports `not_assigned_or_running` and never reaches the
assignment check.

The event carries no agent id. Nobody authenticated, so recording an actor would
be a fiction; the `via: "reclaim"` payload field is what tells a reader how the
task moved.
