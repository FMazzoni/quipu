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
