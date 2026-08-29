A projection, not an orchestrator. `qp wave` groups in-flight tasks into four
state buckets — `ready`, `assigned`, `running`, `pending` — in that display
order, and writes nothing.

## The `pending` filter

The first three groups list every task in that state. `pending` does not: a
`pending` task appears here **if and only if** it has at least one unresolved
dependency — a `depends_on` task that is not yet `done` or `cancelled`. A
`pending` task with nothing actually blocking it stays out of the view entirely
(`wave_lists_pending_tasks_that_have_unresolved_deps`,
`wave_excludes_pending_task_without_unresolved_deps`).

The distinction the filter is drawing is *stuck* versus *not yet promoted*. Those
look identical in the `state` column and want opposite reactions from a
coordinator, so the wave view answers only the first.

## Structural, not tag-based

The definition above is deliberately broader than the skill-layer `kind:blocker`
tag convention: any unresolved dep qualifies, tagged or not. Two consequences,
both intended:

- A skill that uses its own tag taxonomy cannot desync this view. The binary
  never reads the tag, so there is no second source of truth to fall out of step
  with the dep graph.
- Conversely, a task tagged `kind:blocker` with no dep edge is **not** blocked as
  far as the binary is concerned. The tag is a `qp list --tag` filter handle; the
  edge is the fact.

This is the CLAUDE.md boundary in miniature. Nothing here knows what a wave, a
critique loop, or a branch-and-evaluate is — those live in `skills/`. The name
`wave` is borrowed from the pattern that motivated the view, but the question the
command answers is purely a graph question, and it stays answerable if the
pattern is retired tomorrow.

## Stale holds

Each row carries `stale` and the `hold` behind it, and the human rendering trails
a `[STALE …]` note after the title of any row whose holder has gone quiet past
its lease. A stranded ticket has to be legible in the view an orchestrator
already reads: needing a special command to find one means nobody finds one.

The marker trails the title rather than taking a column of its own. The four
columns here line up with what `list` and `show` print, and a fifth would be a
positional change every reader of those views inherits.

The staleness itself is `lease::holds_by_task`, one query for the page — a
per-row statement would put a `SELECT` per ticket on the most-run view in the
system — and the predicate behind it is the one `reclaim` guards on, so this view
never calls something stale that `reclaim` would refuse to release.
