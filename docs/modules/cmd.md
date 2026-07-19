Every module here is one `qp` subcommand, and almost every one is one of two
things: an **edge** in the task state machine, or a **projection** — a read-only
view over the rows those edges wrote. Which of the two a module is predicts most
of what matters about it: how it is reviewed, and how much damage it can do. The
alphabetical table below carries none of that signal, so read this page first.

## The lifecycle, and which module is which edge

A task moves through six states. Terminal states are `done` and `cancelled`;
everything else is in flight.

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

There is no `pending` → `ready` command, and that absence is deliberate.
Promotion has exactly one implementation — `refresh_ready` in `db.rs` — which
every command that might resolve a dependency calls before returning. It is why
`abandon` and `reclaim` both route through `pending` instead of deciding for
themselves whether a released task is ready again: one rule in one place, so a
release path cannot come to disagree with the promotion path about what "ready"
means.

Three mutators are not state edges at all. [`edit`](edit/index.html),
[`tag`](tag/index.html) and [`log`](log/index.html) change a task's fields, its
labels, or its history without moving it through the graph. They still write
events, and `edit` still refuses to touch a terminal task.

`log` carries one rule worth knowing before you use it: with no `--as`, it
auto-attributes the entry to the task's latest open assignee, but **only while
the task is `running`**. That is the one state where the owner is unambiguous —
`assigned` means somebody was nominated but has not started, and a released task
has no owner at all. Outside `running` the entry is recorded with no agent rather
than with a guess. An explicit `--as` always wins
(`log_auto_attributes_to_running_assignee`, `log_explicit_as_always_wins`).

## Mutators and projections

| | modules | risk |
|---|---|---|
| **Mutators** | `add`, `assign`, `claim`, `complete`, `abandon`, `reclaim`, `cancel`, `block`, `depends`, `edit`, `tag`, `log`, `relation` | all of it |
| **Projections** | `list`, `tree`, `show`, `status`, `wave`, `timeline`, `decisions`, `watch`, `report`, `wait` | none — read-only |

The asymmetry is the point, and it runs opposite to file size.
[`report`](report/index.html) is the largest module in this directory and the
least dangerous one: it opens the database, reads, and prints. Its worst failure
is a wrong number on a status page — visible to whoever reads it, and fixed by
re-running. [`claim`](claim/index.html) is among the smallest, and its worst
failure hands one task to two agents who then edit the same files.

So scrutiny follows risk, not line count. A projection's review question is "is
this query right". A mutator's is the four-item checklist below, and it is worth
applying to all thirteen.

Two projections make promises about their *shape* that are easy to break by
accident. [`status`](status/index.html) always emits every known state, including
the ones with a count of zero, so a consumer can index into the result without
existence checks and a state that empties out does not vanish from the output
mid-run (`status_shows_all_states_including_zero`). [`tree`](tree/index.html)
scoped to a root task returns that task's transitive dependency subtree
*inclusive of the root* — the root is part of its own subtree, which is the
convention `report --wave` shares, so the two agree on what a wave contains.

## The four-part invariant every mutator follows

```rust,ignore
db::with_tx(&mut conn, |tx| {                     // 1. BEGIN IMMEDIATE
    let n = tx.execute(
        "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",   // 2. guarded
        rusqlite::params![db::State::Running, task_id, db::State::Assigned])?;
    if n != 1 {                                   // 3. exactly-one check
        return Err(db::conflict("state_changed_under_us", "...", Some(display_id)));
    }
    db::insert_event(tx, ..., "state_change", ...)?;                 // 4. event
    Ok(())
})?;
```

1. **`with_tx` opens `BEGIN IMMEDIATE`**, taking SQLite's write lock at the top
   of the transaction rather than at the first write. Writers serialize, and
   nothing can interleave between a read and a write inside the closure. A
   deferred transaction would let two processes both read, both decide to
   proceed, and then fail one of them at commit — after it had already acted on
   its own decision.
2. **The `WHERE` guards the state.** The transition applies only if the task is
   still where the caller believed it was. Multi-state guards stay spelled as
   SQL literals (`state IN ('assigned','running')`) because they do not
   parametrise idiomatically; the value being *written* is always a bound
   `db::State`, so the vocabulary has one definition.
3. **`n != 1` is checked, always.** This is the line that makes a race safe: two
   agents claiming the same task produce exactly one winner and one `conflict`,
   pinned by `concurrent_claims_produce_exactly_one_winner` and
   `concurrent_assigns_produce_exactly_one_winner` in `tests/race.rs`. Drop the
   check and the loser reports success having matched zero rows.
4. **An event is written in the same transaction.** The change is never
   invisible, and because events are only ever inserted under the write lock,
   `event.id` is gap-free as readers see it — which is what makes `watch`'s
   `WHERE id > last_seen` correct.

Read-then-write is banned as policy, not as style. Where a read genuinely cannot
be avoided it happens *inside* the `IMMEDIATE` transaction, where the write lock
already makes it safe: `abandon` and `reclaim` read the resulting state back for
their event payload, which is auxiliary data and never control flow.

Losing a race is a normal outcome, not a crash. `conflict` exits **2**, and its
code string — `not_ready`, `already_claimed`, `state_changed_under_us` — tells a
calling skill whether to retry or escalate. Exit 1 means the input was wrong;
exit 2 means the store refused. Ownership failures are a separate variant
(`not_owner`, also exit 2) rather than a conflict code, because "you are not the
assignee" is never worth retrying, and `block_wrong_agent_yields_not_owner_not_conflict`
pins the distinction.

## What deliberately is not here

- **Orchestration patterns.** Nothing in this directory knows what a wave, a
  critique loop, or a branch-and-evaluate is. The patterns live in `skills/`.
  [`wave`](wave/index.html) is only a projection that groups in-flight tasks by
  state, and the rule it uses is deliberately structural: a `pending` task shows
  up as blocked **if and only if** it has at least one dependency that is not yet
  `done` or `cancelled`. That is broader than the skill-layer `kind:blocker` tag
  convention — any unresolved dep qualifies, tagged or not — so a skill using its
  own taxonomy cannot desync the view, and conversely a task tagged as a blocker
  with no dep edge is not blocked as far as the binary is concerned. A `pending`
  task with no unresolved deps stays out of the wave view entirely
  (`wave_lists_pending_tasks_that_have_unresolved_deps`,
  `wave_excludes_pending_task_without_unresolved_deps`).
- **Rendering beyond a line of text.** Markdown and HTML report rendering used to
  live in `report`. It now lives in the `report-render` skill, which consumes
  `qp report --json`.
- **The choice between prose and JSON output.** A mutator builds one `Outcome`
  struct and hands it to `emit`, which picks the representation. That is why
  `--json` is not a second code path and cannot drift from the human one.
- **Canonical read queries.** Those belong in `store.rs`. Guarded `UPDATE`s
  deliberately stay here: each has a distinct `WHERE`/`SET`, so they are not
  duplicated with one another, and relocating the highest-stakes code in the
  project for taxonomic tidiness would be a bad trade.
