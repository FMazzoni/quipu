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

That invariant is *emergent, not enforced*. It holds only because every
command that moves a task out of `assigned`/`running` also closes the
assignment in the same transaction — `abandon`, `reclaim`, `block`,
`cancel`, `complete` — and because this module is the only `INSERT INTO
assignment` in the tree. Nothing in `schema.sql` enforces it: there is no
partial unique index on `assignment(task_id) WHERE completed_at IS NULL`.
A new command that demotes a task without closing its assignment, or a
reordering inside any of those five, silently makes this branch live.

So the guard stays. Deleting it would trade a cheap conditional INSERT for
a silent second open assignment row, which breaks `db::current_assignment`'s
"at most one" premise and makes latest-open and latest-by-id disagree — a
corruption that surfaces far from its cause. `tests/cli.rs`'s
`open_assignment_implies_assigned_or_running` pins the premise: if it ever
fails, this branch is no longer defensive and the failure is the warning.
