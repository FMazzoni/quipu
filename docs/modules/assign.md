Orchestrator-only. Agents take work with `qp claim`, never this.

## The `stale_open_assignment` guard

The `INSERT INTO assignment` is conditional on no open (`completed_at IS NULL`)
row existing for the task, and reports `stale_open_assignment` when that
condition fails. No CLI sequence reaches it: an open assignment row exists only
while the task is `assigned` or `running`, and the `state = ready` guard on the
`UPDATE` above runs first, so two racing `qp assign` processes make the loser
report `not_ready`.

That invariant used to be emergent. It held because `assign.rs` is the only
`INSERT INTO assignment` in the tree, and because the five commands that move a
task out of `assigned`/`running` — `abandon`, `reclaim`, `block`, `cancel`,
`complete` — each close the assignment in the same transaction.

QP-142 made it structural. `schema.sql` carries `idx_assign_one_open`, a partial
unique index on `assignment(task_id) WHERE completed_at IS NULL`, so SQLite
rejects a second open row. A command that demotes a task without closing its
assignment fails at the write rather than corrupting the store.

The guard's role therefore changed rather than vanished: it converts a raw
SQLite `UNIQUE constraint failed` into the typed `stale_open_assignment`
conflict, which exits 2 with a code string a skill can branch on. The index is
the enforcement; this branch is the diagnosis. It also still covers stores
created before the index landed, where nothing else protects
`db::current_assignment`'s "at most one open row" premise.

`tests/cli.rs` pins both halves: `open_assignment_implies_assigned_or_running`
walks the commands that must close their rows, and
`unique_index_rejects_second_open_assignment` pins that the storage layer refuses
the write on its own.
