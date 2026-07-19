Cycle-checked before insert. Adding or removing an edge can change what
is ready, so both paths re-derive readiness.

## The two directions are not mirror images

Adding an edge can only ever *demote*, and removing one can only ever *promote*.
That asymmetry is why the two branches look so different, and why the outcome's
`promoted` field is always `false` on add.

The demotion is itself a guarded `UPDATE` with the unresolved-dep predicate
inline: it matches only a `ready` task that now genuinely has an open
prerequisite. So it is a no-op when the new dep is already `done` — no spurious
demotion, no event (`depends_demote_emits_state_change_event` covers the case
where the demotion does happen).

The promotion cannot work that way, because `refresh_ready` promotes tasks in
bulk and reports nothing about which ones moved. Removing an edge can free tasks
other than the one named, so the branch snapshots the promotion candidates first,
calls `refresh_ready`, then re-reads each candidate to see which actually landed
in `ready` and emits an event for it. Without the snapshot the promotions would
be silent, and a `watch` consumer would see a task become workable with nothing
in the log explaining why (`depends_rm_emits_state_change_on_promote`).

## Why the *downstream* task is the one whose owner is checked

Adding a dependency to a task somebody is actively working on can yank it out
from under them, so `--as` is required and must match the assignee when the
downstream task is `assigned` or `running`
(`depends_on_running_task_requires_matching_agent`). The upstream task is not
mutated at all — its state never changes — so demanding its owner's consent would
block legitimate scheduling on a task nobody is touching. Ownership follows the
row being written.

That ownership check reads the *latest open* assignment, not the latest by id.
`depends_uses_latest_open_assignment_not_latest_by_id` exists because the two
diverge as soon as a task has been released once, and the wrong one would let a
previous assignee keep authority it no longer holds.

## Boundaries

Adding an edge that already exists is an idempotent success, not an error — the
`INSERT OR IGNORE` matches nothing and the command returns early without an
event, so replaying a setup script does not fill the log with noise. Removing one
that does not exist *is* an error (`not_found`), because it means the caller's
model of the graph is wrong.

Cycle prevention is `would_cycle` in `db.rs`, a recursive CTE run inside the
transaction. It has to be there rather than in `store.rs`: it must see edges this
transaction has already inserted (`depends_rejects_transitive_cycle`).
