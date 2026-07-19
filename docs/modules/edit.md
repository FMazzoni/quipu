One `edit` event is emitted when a field actually changes; a no-op edit emits
nothing.

## Conditional event

Every field is diffed against its current value first, and the `UPDATE` is built
from only the fields that differ. If nothing differs the transaction returns
before the `UPDATE`, so re-running the same `qp edit` reports `no changes` and
writes neither a row nor an event (`edit_no_op_skips_event`). The event log is
the audit spine: a retry-happy agent re-applying its own edit should not
manufacture a history of changes that never happened.

The event payload carries `from` and `to` for each changed field, so the log is
reconstructable — you can tell what a description used to say without a snapshot.

## The diff read

Diffing requires reading current values, which looks like the read-then-write
this crate bans. It is safe because the read happens inside the `IMMEDIATE`
transaction, and because the `UPDATE` is still guarded — on the terminal-state
predicate rather than on the values read. No concurrent write can slip between
the read and the write, and the count is still checked.

## Boundaries

The `UPDATE` carries `state NOT IN ('done','cancelled')`, so editing a terminal
task reports `not_editable` (`edit_rejected_on_done_task`,
`edit_rejected_on_cancelled_task`). A finished task's title is part of the record
other tickets and events refer to. `running` is not excluded — an agent refining
its own ticket mid-flight is normal work (`edit_during_running_state_allowed`).

An empty string clears a nullable field rather than storing `""`, so `--tier ''`
and `--description ''` unset those columns
(`edit_can_clear_tier_with_empty_string`). `--title` has no such reading: empty
is rejected as `invalid_input` before the connection opens. Passing none of
`--title`, `--tier`, `--description` is also `invalid_input`, exit 1, rather than
a silent success.

State is not editable here. Every state change is an edge with its own command
and its own guard; a general-purpose field editor that could also write `state`
would be a hole straight through the state machine.
