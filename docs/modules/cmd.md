Each module here is one `qp` subcommand. Every one but
[`install_skills`](install_skills/index.html) is either an **edge** in the task
state machine or a **projection** — a read-only view over the rows those edges
wrote. `install_skills` touches no database; it copies or symlinks the bundled
`skills/` into Claude Code's skill directory.

## Task lifecycle

`db::State` has six variants. Terminal states are `done` and `cancelled`.

```text
                   assign            claim           complete
  pending ──▶ ready ──────▶ assigned ──────▶ running ──────────▶ done
     ▲                           │               │
     │  refresh_ready            └───────────────┘
     │  (deps resolved)            abandon / reclaim
     └─────────────────────────────────┘

  any non-terminal ──cancel──▶ cancelled
```

| module | edge |
|---|---|
| [`add`](add/index.html) | nothing → `ready`, or `pending` when created with unresolved deps |
| [`assign`](assign/index.html) | `ready` → `assigned` |
| [`claim`](claim/index.html) | `assigned` → `running` |
| [`complete`](complete/index.html) | `running` → `done` |
| [`abandon`](abandon/index.html) | `assigned`/`running` → `pending`, agent-side (ownership checked) |
| [`reclaim`](reclaim/index.html) | `assigned`/`running` → `pending`, orchestrator-side (no ownership check) |
| [`cancel`](cancel/index.html) | any non-terminal → `cancelled` |
| [`block`](block/index.html) | `assigned`/`running` → `pending`, plus a new blocker task and a dep edge |
| [`depends`](depends/index.html) | `ready` → `pending` when an edge is added; `pending` → `ready` when one is removed |

## Promotion to ready

There is no `pending` → `ready` command. Promotion has exactly one
implementation, `refresh_ready` in `db.rs`, which every command that might
resolve a dependency calls before returning. That is why `abandon` and `reclaim`
both write `pending` rather than deciding for themselves whether a released task
is ready again: a release path cannot come to disagree with the promotion path
about what "ready" means.

## Non-edge mutators

[`edit`](edit/index.html), [`tag`](tag/index.html), [`log`](log/index.html) and
[`relation`](relation/index.html) change a task's fields, labels, history or
links without moving it through the graph. They still write events, and `edit`'s
`UPDATE` carries `state NOT IN ('done','cancelled')`, so a terminal task is not
editable.

With no `--as`, `log` attributes the entry to the open assignee only when the
task is `running`; otherwise the event carries no agent. See
[`log`](log/index.html) for why the rule is drawn there.

## Mutators and projections

| | modules | risk |
|---|---|---|
| **Mutators** | `add`, `assign`, `claim`, `complete`, `abandon`, `reclaim`, `cancel`, `block`, `depends`, `edit`, `tag`, `log`, `relation` | all of it |
| **Projections** | `list`, `tree`, `show`, `status`, `wave`, `timeline`, `decisions`, `watch`, `report`, `wait` | none — read-only |

Risk runs opposite to file size. [`report`](report/index.html) is the largest
module in this directory and cannot corrupt anything: it opens the database,
reads, and prints, and its worst failure is a wrong number on a status page.
[`claim`](claim/index.html) is a quarter of its size, and its worst failure hands
one task to two agents who then edit the same files. Scrutiny follows risk, not
line count.

## Projection shape guarantees

Two projections promise a *shape* that is easy to break by accident.
[`status`](status/index.html) emits every known state including those with a
count of zero, so a consumer can index into the result without existence checks
and a state that empties out does not vanish mid-run
(`status_shows_all_states_including_zero`). [`tree`](tree/index.html) scoped to a
root returns that root's transitive dependency subtree *inclusive of the root* —
both it and `report --wave` call `store::subtree_ids`, so the two agree on what a
wave contains.

## Guarded transitions

Every mutator follows the four-part invariant set out under "The invariant that
repeats everywhere" in the crate-root [architecture doc](../index.html):
`with_tx` opens `BEGIN IMMEDIATE`, the `UPDATE` carries a state guard in its
`WHERE`, the affected-row count is checked against 1, and an event is written in
the same transaction. `tests/race.rs` pins the count check with
`concurrent_claims_produce_exactly_one_winner` and
`concurrent_assigns_produce_exactly_one_winner`.

Where a read cannot be avoided it happens *inside* the `IMMEDIATE` transaction:
`abandon` and `reclaim` read the resulting state back for their event payload,
which is auxiliary data and never control flow.

Losing a race is a normal outcome. `conflict` exits **2**, and its code string —
`not_ready`, `already_claimed`, `state_changed_under_us` — tells a calling skill
whether to retry or escalate. Exit 1 means the input was wrong; exit 2 means the
store refused. Ownership failures are a separate variant (`not_owner`, also exit
2) rather than a conflict code, because "you are not the assignee" is never worth
retrying; `block_wrong_agent_yields_not_owner_not_conflict` pins the distinction.

## Out of scope

- **Orchestration patterns.** No module here knows what a wave, a critique loop,
  or a branch-and-evaluate is; the patterns live in `skills/`.
  [`wave`](wave/index.html) is named after one but is only a projection that
  groups in-flight tasks by state, using a structural rather than tag-based rule
  for what counts as blocked.
- **Rendering beyond a line of text.** Markdown and HTML report rendering lives
  in the `report-render` skill, which consumes `qp report --json`.
- **Choosing between prose and JSON.** A mutator builds one `Outcome` struct and
  hands it to `emit`, which picks the representation, so `--json` is not a second
  code path.
- **Canonical read queries.** Those belong in `store.rs`. Guarded `UPDATE`s stay
  here: each has a distinct `WHERE`/`SET`, so they are not duplicated with one
  another.
