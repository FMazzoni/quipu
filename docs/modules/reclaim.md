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

## Stale by default, live only under force

Releasing a hold that has gone quiet past its lease needs no flag. Releasing one
that is still live needs the force flag, and reports `hold_is_live` naming the
holder, the machine and the instant it would expire. That is the whole shift this
command underwent: the manual override stays, and it stops being the only option.

A task sitting `assigned` and never claimed has no holder, so no force is needed
there — nobody had started.

Staleness is part of the guarded `UPDATE`, not a check in front of it. The
predicate is `lease::LIVE_HOLD`, embedded in the `WHERE` clause as a correlated
`NOT EXISTS`, so one conditional statement and one `changes() == 1` decide both
that the task was in flight and that its hold had lapsed. Two machines racing to
break the same dead hold therefore produce exactly one winner — the loser finds
the task already `pending` and reports `not_assigned_or_running` — and a hold
renewed in the moment between reading and writing survives, because there is no
read to race with. `concurrent_reclaims_of_one_stale_hold_produce_exactly_one_winner`
pins it.

The state read after a failed `UPDATE` is for the message only, the same
permitted exception the origin read above already takes: it decides which of the
two refusals to phrase, never whether the transition happened.
