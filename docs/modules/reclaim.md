Force-release with no ownership check, for when an agent has died and
cannot release its own claim. Compare `abandon`.

The missing ownership check is the entire point, and it is why this is a separate
command rather than a `--force` flag on `abandon`. An orchestrator recovering
from a crashed subagent has no way to prove it is that agent, and should not have
to impersonate one; a flag would put that power one typo away from every agent
that meant to release its own work. Separate verbs make the dangerous one
something you have to reach for deliberately
(`reclaim_force_releases_without_agent_id`).

Everything downstream matches `abandon`: the task goes to `pending`,
`refresh_ready` decides whether it comes straight back to `ready`, and the
resulting state is read back for the event payload rather than assumed
(`reclaim_returns_to_pending_when_unresolved_dep_exists`).

Two small asymmetries against `abandon`, both deliberate. The assignment close
targets *every* open row for the task rather than one known row id — this is a
cleanup path, and if an earlier bug left two open assignments, reclaim is the
command that should mop them up rather than trip over them. And it therefore
checks for zero rather than for exactly one: closing several is a success here,
whereas closing none means there was nothing to reclaim. Note that the guarded
`UPDATE` runs first, so a task that is not `assigned` or `running` reports
`not_assigned_or_running` and never reaches the assignment check.

The event carries no agent id. Nobody authenticated, so recording a claimed
actor would be a fiction; the `via: "reclaim"` payload field is what tells a
reader how the task moved.
