# quipu architecture

How `qp` is put together, and why. This is the *shape* of the system — the parts
that survive refactors. Per-file detail lives in `//!` headers next to the code.

> **This document is written by an LLM about code it also wrote.** Treat it as a
> map, not as ground truth. To check it, run the `qp-verify-docs` skill, which
> reads each source file and reports where its documentation has drifted; every
> discrepancy becomes a `kind:docs` ticket. If you find a statement here that is
> wrong, that is a bug — file it.
>
> Claims about *behaviour* should be traceable to something you can check
> yourself: the test suite in [`tests/`](../tests), or a command you can run by
> hand. Claims about *internal structure* are the ones most likely to rot.

## What quipu is

A task substrate for coordinating parallel agents. It is deliberately **not** an
orchestrator: it holds state and enforces safe transitions, while orchestration
patterns (wave, critique-loop, branch-and-evaluate) live in
[`skills/`](../skills). The binary stays pattern-agnostic.

Everything is one SQLite file per project, and one process per command. No
daemon, no server, no async runtime.

## Three concepts

Almost everything in the codebase is one of these three things.

### 1. A state machine, one per task

```text
                   assign            claim           complete
  pending ──▶ ready ──────▶ assigned ──────▶ running ──────────▶ done
     ▲          ▲                │               │
     │          │                └───────────────┘
     │          │                  abandon / reclaim
     │          │                        │
     │          └──── refresh_ready ─────┘
     │                (deps resolved)    ▼
     └──────────────────────────────── pending

  any non-terminal ──cancel──▶ cancelled
```

Terminal states are `done` and `cancelled`. Everything else is in flight.

**Every mutating command is exactly one edge in this graph.** That is why those
files are small — an edge is a small thing. [`assign.rs`](../src/cmd/assign.rs)
is `ready → assigned`; [`claim.rs`](../src/cmd/claim.rs) is
`assigned → running`.

Release paths (`abandon`, `reclaim`) both return the task to `pending` rather
than guessing whether its dependencies still hold. `refresh_ready` then promotes
it to `ready` when they do. One rule, one place.

### 2. A DAG that decides `pending` vs `ready`

The `dep` table. Exactly one function computes readiness — `refresh_ready` in
[`db.rs`](../src/db.rs) — and any command that might resolve a dependency calls
it. A task is `ready` when no dependency is left in a non-terminal state.

Cycle prevention lives in `would_cycle`, a recursive CTE in the same file.

### 3. An append-only event log

Every mutation writes a row to `event` via `insert_event`. This is the audit and
forensics spine: [`timeline`](../src/cmd/timeline.rs),
[`decisions`](../src/cmd/decisions.rs), and [`watch`](../src/cmd/watch.rs) are
all just queries over it.

Events are only ever inserted inside the same transaction as the mutation they
describe. Because writers serialize (see below), `event.id` is gap-free — which
is what makes `watch` correct.

## The invariant that repeats everywhere

This is the heart of the system. Read it once and ~13 files become the same file.

```rust,ignore
db::with_tx(&mut conn, |tx| {              // 1. BEGIN IMMEDIATE — take the write lock now
    let n = tx.execute(
        "UPDATE task SET state = 'X' WHERE id = ? AND state = 'Y'",  // 2. guarded edge
        [task_id])?;
    if n != 1 { return Err(db::constraint(...)); }                   // 3. exactly-one check
    db::insert_event(tx, ..., "state_change", ...)?;                 // 4. audit trail
    Ok(())
})?;
```

Four parts, and each one is load-bearing:

1. **`BEGIN IMMEDIATE`** (`with_tx` in [`db.rs`](../src/db.rs)) takes SQLite's
   write lock at transaction start rather than at first write. Writers therefore
   serialize, and no other process can interleave between a read and a write
   inside the block.
2. **The guarded `WHERE`** means the transition only applies if the task is
   still where the caller thought it was.
3. **`n != 1`** means you *find out* when it wasn't, instead of silently
   clobbering. This is what makes a claim atomic: two agents racing the same
   task produce exactly one winner and one constraint error.
4. **The event** means the change is never invisible.

Read-then-write is banned as a matter of policy. Where a read is unavoidable, it
happens inside the `IMMEDIATE` transaction, where it is safe.

**Reviewing a mutating command is a four-item checklist**: is it in a `with_tx`,
is the `UPDATE` guarded, is the count checked, does it emit an event. You do not
need deep Rust fluency to apply that.

## Mutators vs projections

The most useful split in the codebase, and it is not visible from the file tree.

| | what they are | risk |
|---|---|---|
| **Mutators** | `add`, `assign`, `claim`, `complete`, `abandon`, `reclaim`, `cancel`, `block`, `depends`, `edit`, `tag`, `log`, `relation` | **all of it** |
| **Projections** | `list`, `tree`, `show`, `status`, `wave`, `timeline`, `decisions`, `watch`, `report`, `wait` | none — read-only |

Projections cannot corrupt anything. [`report.rs`](../src/cmd/report.rs) is by
far the largest file in the crate and is also the least dangerous — worst case it
renders something wrong. Meanwhile [`claim.rs`](../src/cmd/claim.rs) is one of
the smallest, and if it is wrong you double-dispatch work to two agents.

Scrutiny should follow risk, not line count.

## Layering

```text
main.rs      clap subcommand enum → dispatch. Routing only.
  db.rs      connection, PRAGMAs, migrations, transactions, guarded-transition helpers
  store.rs   canonical read queries + the row types they return
  cmd/*.rs   argument parsing and rendering
```

[`store.rs`](../src/store.rs) exists because the same read queries were
hand-written across many command files in subtly divergent forms. Its own module
header explains the scope rules — notably that **guarded-transition `UPDATE`s
deliberately do not move there**, since each has a distinct `WHERE`/`SET` and
relocating the highest-stakes code for tidiness would be a bad trade.

This layering is **in progress**, not finished. `store.rs` is being populated
incrementally, and `cmd/*.rs` files still contain SQL that has not migrated yet.

## Storage

One SQLite file, default `.quipu/db.sqlite`, discovered by walking up from the
cwd — with a git-aware fallback so that commands run inside a worktree find the
main repo's store.

PRAGMAs set on every open, in [`db.rs`](../src/db.rs):

| pragma | value | why |
|---|---|---|
| `journal_mode` | `WAL` | readers don't block the writer |
| `synchronous` | `NORMAL` | safe under WAL, much faster than `FULL` |
| `foreign_keys` | `ON` | referential integrity is enforced, not assumed |
| `busy_timeout` | `5000` | wait for a contended write lock instead of failing |

Tables: `meta`, `default_tag`, `task`, `dep`, `assignment`, `event`, `tag`,
`relation`. See [`schema.sql`](../src/schema.sql), which is the authority.

Two notes on the schema as it stands:

- `task.state` is bare `TEXT`. The legal values are documented in a comment and
  enforced by the guarded `WHERE` clauses, but **there is no `CHECK`
  constraint** — the domain is not enforced by the database itself.
- `created_at` and friends default to SQLite's `strftime`, while Rust-side
  timestamps come from `now_rfc3339` in [`time.rs`](../src/time.rs). These
  produce different sub-second precision, so cross-table lexicographic time
  comparison is not reliable.

## Identifiers

Tasks carry a rowid and a display id (`QP-1`), formatted by
[`id.rs`](../src/id.rs). The prefix is per-store, fixed at `qp init`, default
`QP`. `resolve` matches on the display-id string, so the prefix in user input is
informational.

## Exit codes

Set in [`main.rs`](../src/main.rs). Agents branch on these, so they are a
contract:

| code | meaning |
|---|---|
| 0 | success |
| 1 | generic error / invalid input |
| 2 | constraint violation — wrong state, wrong assignee, already claimed |
| 3 | `wait` timed out |
| 4 | `wait --cohort-done` matched an empty cohort |

Code 2 is the interesting one: it is the *expected* outcome of losing a race, not
a failure.

## Barriers

`qp wait --cohort-done --tag <t>` blocks until the tag-matched cohort has
`total > 0` and no task left in a non-terminal state. The empty-cohort case is a
distinct exit code (4) rather than a silent success, because "nothing matched"
and "everything finished" must not look alike to an orchestrator.

## A reading path

To understand the system, read along the state machine rather than the file
tree — the alphabetical `cmd/` listing carries no signal.

1. `with_tx` and `insert_event` in [`db.rs`](../src/db.rs) — the two primitives
   everything else uses.
2. [`add.rs`](../src/cmd/add.rs) → [`assign.rs`](../src/cmd/assign.rs) →
   [`claim.rs`](../src/cmd/claim.rs) → [`complete.rs`](../src/cmd/complete.rs),
   in that order. That is one task's whole life.
3. [`abandon.rs`](../src/cmd/abandon.rs) and
   [`reclaim.rs`](../src/cmd/reclaim.rs) — the recovery edges.
4. `refresh_ready` and `would_cycle` in [`db.rs`](../src/db.rs) — the two DAG
   operations.

Skip everything else until you need it.

[`tests/cli.rs`](../tests/cli.rs) is also worth reading early: the tests drive
the real binary and assert on its observable behaviour, so they read closer to
shell scripts than to Rust, and they are the executable specification for
everything above.
