A new task lands in `ready` when it has no unresolved deps, `pending` otherwise.

## Initial state, decided twice

The `INSERT` picks `ready` or `pending` from whether any deps were named at all.
Then each dep goes in through `link_dep`, which runs the logged readiness sweep
and promotes the row back to `ready` when every one of those deps is already
`done` or `cancelled`. `add_with_deps_starts_pending_then_unblocks` covers the
case where the deps are genuinely open.

Both steps are in the log, and that is a change. The command used to write a
single `state_change` naming the state read back at the end, which collapsed
"born `pending`, then promoted" into one event claiming the task had always been
`ready`. Now the creation event names the state the row was born in and the
promotion gets its own event with both ends of the move, so the sequence replays
to the same place the table is in. The reported state is still the one read back
at the end, because that is the state the caller's next command will meet.

## Everything a create writes, it now says

A create is the one command that can populate four different tables, and for
most of the project's life it announced one of them. Deps went in through a raw
`INSERT` that bypassed `link_dep` and its `dep_added` event; tags went in through
a raw `INSERT OR IGNORE` with no event at all; tier and description were set by
the `INSERT` itself and mentioned nowhere. Since `--tag` is how essentially every
tag in a store arrives, the log knew about almost none of them.

So: the creation `state_change` carries `title`, and `tier` and `description`
when they were given; each dep is written by `link_dep` and gets its `dep_added`;
each tag that lands gets the same `tag_added` event `qp tag add` writes. The
creation event is emitted *first*, before any edge or tag referencing the new
task, because a log whose first mention of a task is an edge pointing at it
cannot be replayed in order.

Its `from` is `null` — explicitly, not by omission. A task has no prior state,
which is a different claim from an event too old to have recorded one, and the
two need to stay distinguishable.

Routing deps through `link_dep` also makes a repeated `--depends-on QP-1
--depends-on QP-1` a no-op instead of a `UNIQUE` constraint failure, since the
shared helper writes the edge with an `ON CONFLICT` clause.

## `--depends-on` and `--part-of` point opposite ways

Both write `dep` rows and both concern the new task, but they put it on opposite
ends of the edge:

- `--depends-on X` — the new task waits for `X`. The new task is the depender,
  and starts `pending`.
- `--part-of X` — `X` waits for the new task, because the new task is one of the
  pieces `X` is made of. `X` is the depender.

So `--part-of` puts the *named* task on the waiting end. Getting it backwards
produces a graph that looks plausible and rolls up exactly wrong, which is why
`add_part_of_attaches_to_a_container` pins the container's state down.

The new task usually starts `ready`, but not always: if `X` is itself blocked,
the new task is inside a frozen container and arrives `pending`. Creating a task
can therefore land it straight into a freeze it had no part in
(`a_slice_added_to_a_frozen_wave_arrives_frozen`).

The container is the row being written, so its owner gates the write: `--as`
must match when the container is `assigned` or `running`. Linking still cannot
demote a `running` container — the guarded `UPDATE` matches `ready` only — so
filing a slice never yanks work out of a live agent's hands.

## Dep resolution and cycle checking

Dependency references are resolved before the transaction opens, so a typo'd id
fails without ever taking the write lock. Cycle checking cannot move out with
them: `would_cycle` has to see edges inserted earlier in this same transaction,
which is why a self-dependency is caught here rather than at parse time
(`add_rejects_cycle_on_self_dep`).

## Display id

The display id is written by a second `UPDATE` because it is derived from the
rowid, which SQLite does not hand out until the `INSERT` has happened. Both
statements are in one transaction, so no reader observes the empty-string
placeholder.

## Tag merging

Store-level `--default-tag` values merge with `--tag` through a `HashSet` rather
than a concatenation, so naming a tag that is already a default yields one tag
and not a duplicate (`default_tag_dedupes_against_explicit_tag`). `block` differs:
passing `--tag` there replaces its default rather than adding to it. Defaults
here are the store's standing policy, and a caller adding a label is not
overriding it.
