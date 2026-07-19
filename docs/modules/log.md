Not a state edge. `log` appends a row to the append-only `event` table without
moving the task through the graph — the same table, and the same transaction
discipline, that every state edge writes to. `--kind` is free-form: the binary
defines no vocabulary for it beyond the kinds its own commands emit
(`state_change`, `blocker`, `dep_added`, …), so a skill can invent its own and
`timeline --kind` will filter on it.

## Auto-attribution happens only while the task is `running`

With no `--as`, the entry is attributed to the task's latest open assignee — but
**only if the task is currently `running`**. That is the one state where the
owner is unambiguous. `assigned` means somebody was nominated but has not
started, and a released task has no owner at all; guessing in either case would
put a name on a log entry that nobody actually wrote. Outside `running` the entry
is recorded with a null agent rather than with a guess.

An explicit `--as` always wins, in every state — the auto path is a convenience
for the common case (a subagent logging against the ticket it holds), never an
override. `log_auto_attributes_to_running_assignee` and
`log_explicit_as_always_wins` pin both halves.

The lookup deliberately reads the *latest open* assignment rather than the latest
assignment by id. See `docs/modules/list.md` for why those two must not be
swapped: the agent column in `qp list` is the latter, and using it here would let
a former assignee's name land on someone else's entry.

## `--auto` is a payload flag, not an attribution flag

`--auto` sets `auto: true` inside the event payload and has nothing to do with
`--as`. It marks the entry as machine-generated so `qp decisions --auto-only`
can select it — the friction-note feed subagents write at the end of a slice. The
two flags are orthogonal and the similar names are the only thing connecting
them.
