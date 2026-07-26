`qp contains <PARENT> <CHILD>...` records that `PARENT` is made up of the named
children; `--rm` releases them. Unlike `qp depends` it takes any number of
children per invocation, all in one transaction.

The edges are `dep` rows with `mode = 'contains'`, which is the whole of the
difference from `qp depends`. Both modes make the depender wait; they differ in
what that waiting means and, from the propagation change onward, in whether a
blocker on the container reaches the things inside it.

## Direction

`qp contains A B` writes `dep(task_id = A, depends_on_task_id = B)` — the
container depends on its contents. Rollup then falls out of the readiness rule
that already exists: a container cannot be `ready` while anything it is made of
is still open, and completing the last child promotes it, with no second rule
and no bookkeeping. Nesting works for the same reason and to any depth, one
level per completed child.

Reading the graph in the other direction — "what is this part of?" — is
`store::containers_of`, surfaced as `part_of` in `qp show`.

## Why not just `qp depends`

Reverse-dependency already expressed containment before this command existed,
and stores in the wild use it. What they could not do is say so: an edge from a
wave to a slice and an edge from a wave to an unrelated prerequisite were the
same row. A marker on the *task* cannot fix that, because a container with both
slices and a prerequisite has every edge leaving the same node. The mode belongs
to the edge.

Existing stores are unaffected. The column defaults to `blocks`, so everything
written before it existed keeps its old meaning and behaviour, and the new
semantics arrive only when someone runs this command.

## Reclassifying

Re-linking a pair that already exists in the other mode changes it in place
rather than erroring. That is the migration path for a store that expressed
containment as plain deps: re-run `qp contains` over the edges that meant
containment all along. It cannot change whether the container is blocked — both
modes wait — so no state transition happens on that path.

Asking for an edge that is already in the requested mode is an idempotent
success, reported in the outcome's `unchanged` list so a re-run reads as
"nothing to do" rather than as new work. Removing an edge that is not there is
also reported as unchanged rather than raised as an error, which is where this
deliberately differs from `qp depends --rm`: that command moves one named edge
and a miss means the caller's model is wrong, while this one moves a batch and
partial overlap with the existing graph is normal.

## Ownership

Checked once, on the container, before any child is linked — the container is
the row being written, so `--as` must match its assignee when it is `assigned`
or `running`. The children are not mutated. Linking cannot demote a `running`
container either; the guarded `UPDATE` in `db::link_dep` matches `ready` only,
so attaching a slice never yanks work out of a live agent's hands.

## Boundaries

Every child is cycle-checked against the graph as it stands mid-transaction, so
a cycle introduced by the fifth child rejects the whole call rather than leaving
four edges applied and no indication which. Ids are resolved before the
transaction opens, so an unknown id fails as a lookup error.

Both modes emit `dep_added` / `dep_removed` with `mode` in the payload. There is
no separate `contains_added` kind on purpose: a consumer filtering the audit log
on `dep_added` would otherwise silently miss half the graph mutations.
