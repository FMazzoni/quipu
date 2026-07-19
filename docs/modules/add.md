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
