Orchestrator-only. Agents take work with `qp claim`, never this.

# Why the `stale_open_assignment` guard is defensive, not dead

The INSERT below is conditional on no open (`completed_at IS NULL`)
assignment existing for the task, and reports `stale_open_assignment` when
that condition fails. No CLI sequence is known to reach it: the codebase
maintains an invariant that an open assignment row exists only while the
task is `assigned` or `running`, and the `WHERE state = 'ready'` guard above
trips first for every task that satisfies it. Two racing `qp assign`
processes therefore make the loser report `not_ready`, never
`stale_open_assignment`.

That invariant *used to be emergent*. It held only because every command
that moves a task out of `assigned`/`running` also closes the assignment in
the same transaction — `abandon`, `reclaim`, `block`, `cancel`, `complete` —
and because this module is the only `INSERT INTO assignment` in the tree.
That was an inductive argument across eight modules with nothing standing
behind it.

**QP-142 made it structural.** `schema.sql` now carries
`idx_assign_one_open`, a partial unique index on `assignment(task_id) WHERE
completed_at IS NULL`. A second open row is rejected by SQLite itself, so a
new command that demotes a task without closing its assignment now fails at
the write instead of quietly corrupting the store.

So the guard stays, but its role changed rather than vanished. It converts
what would otherwise surface as a raw SQLite `UNIQUE constraint failed` into
the typed `stale_open_assignment` error with an exit code an orchestrating
skill can branch on — the index is the enforcement, this branch is the
diagnosis. Deleting it would leave the corruption caught but unreadable, and
would still break `db::current_assignment`'s "at most one" premise for any
store created before the index landed.

`tests/cli.rs` pins both halves: `open_assignment_implies_assigned_or_running`
walks the commands that must close their rows, and
`unique_index_rejects_second_open_assignment` pins that the storage layer
refuses the write on its own.
