Emits one `edit` event when a field actually changes; a no-op edit emits nothing.

## Why the event is conditional

Every field is diffed against its current value first, and the `UPDATE` is built
from only the fields that differ. Re-running the same `qp edit` therefore reports
`no changes` and writes nothing (`edit_no_op_skips_event`). This matters because
the event log is the audit spine: a retry-happy agent that re-applies its own
edit should not manufacture a history of changes that never happened, and
`qp timeline` should not need to filter out empty diffs.

The event payload carries `from` and `to` for each changed field, which is what
makes the log reconstructable rather than merely suggestive — you can tell what a
description used to say without a snapshot.

## The read here is real, and permitted for a specific reason

Diffing requires reading current values, which looks like the read-then-write
this crate bans. It is safe because it happens inside the `IMMEDIATE`
transaction, and because the `UPDATE` is still guarded — on the terminal-state
predicate rather than on the values read. A concurrent write cannot slip between
the read and the write, and the count is still checked.

## Boundaries

Editing is refused on `done` and `cancelled` tasks (`edit_rejected_on_done_task`,
`edit_rejected_on_cancelled_task`). History is not rewritable: a finished task's
title is part of the record other tickets and events refer to. `running` is
explicitly fine — an agent refining its own ticket mid-flight is normal work, not
a violation (`edit_during_running_state_allowed`).

An empty string clears a nullable field rather than storing `""`, so
`--tier ''` unsets the tier (`edit_can_clear_tier_with_empty_string`). `--title`
has no such reading — a task with no title is not a meaningful row, so empty is
rejected outright. Passing no fields at all is `invalid_input` (exit 1) rather
than a silent success, because it almost always means a flag was misspelled.

State is not editable here. Every state change is an edge with its own command
and its own guard; a general-purpose field editor that could also write `state`
would be a hole straight through the state machine.
