A new task lands in `ready` when it has no unresolved deps, `pending` otherwise.

## Initial state, decided twice

The `INSERT` picks `ready` or `pending` from whether any deps were named at all.
Then, if deps were named, `refresh_ready` runs before the transaction closes and
promotes the row back to `ready` when every one of those deps is already `done`
or `cancelled`. The `state_change` event is written from the state read back
after that promotion, not from the guess made at insert time, so a task created
against already-finished prerequisites emits one event saying `ready` rather than
two that contradict each other. `add_with_deps_starts_pending_then_unblocks`
covers the case where the deps are genuinely open.

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
