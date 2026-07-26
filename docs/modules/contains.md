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
containment all along.

Reclassifying is not state-neutral, and on a blocked container it is the
opposite of quiet. It cannot change whether the *container* is blocked — both
modes wait — but it moves everything below the edge into or out of the frozen
set. Converting a wave that already carries a blocker demotes every `ready`
slice inside it on the spot; converting the other way promotes them back.

Asking for an edge that is already in the requested mode is an idempotent
success. Removing one that is not there — or that exists as a `blocks` edge,
which `--rm` will not touch — is likewise reported rather than raised, which is
where this deliberately differs from `qp depends --rm`: that command moves one
named edge and a miss means the caller's model is wrong, while this one moves a
batch and partial overlap with the existing graph is normal.

The outcome splits the children into `changed` and `unchanged`, and the human
line names only what moved. It has to: saying `released QP-1, QP-2` when one of
them still carries a live `blocks` edge is a false claim about the graph, and
the exit code is 0 either way, so a teardown script would believe it.

Re-running an identical link is not a no-op internally. It skips the write and
still re-derives the frozen set, so re-issuing the command is a real repair for
a store whose readiness drifted.

## Ownership

Checked once, on the container, before any child is linked — the container is
the row being written, so `--as` must match its assignee when it is `assigned`
or `running`.

The children have no say, which is only defensible because of what a link can do
to them. It can move a child from `ready` to `pending`, but never out of
`assigned` or `running`: every demotion here is guarded on `ready`. So a link
can stop the next slice being dispatched and can never take work out of a live
agent's hands, and that is why one check on the container is enough.

## Boundaries

Every child is cycle-checked against the graph as it stands mid-transaction, so
a cycle introduced by the fifth child rejects the whole call rather than leaving
four edges applied and no indication which. Ids are resolved before the
transaction opens, so an unknown id fails as a lookup error.

Both modes emit `dep_added` / `dep_removed` with `mode` in the payload. There is
no separate `contains_added` kind on purpose: a consumer filtering the audit log
on `dep_added` would otherwise silently miss half the graph mutations.
