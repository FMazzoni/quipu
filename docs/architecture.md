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
> yourself: the test suite in [`tests/`](https://github.com/FMazzoni/quipu/tree/main/tests), or a command you can run by
> hand. Claims about *internal structure* are the ones most likely to rot.

## What quipu is

A task substrate for coordinating parallel agents. It is deliberately **not** an
orchestrator: it holds state and enforces safe transitions, while orchestration
patterns (wave, critique-loop, branch-and-evaluate) live in
[`skills/`](https://github.com/FMazzoni/quipu/tree/main/skills). The binary stays pattern-agnostic.

Everything is one SQLite file per project, and one process per command. No
daemon, no server, no async runtime.

## Three concepts

Almost everything in the codebase is one of these three things.

### 1. A state machine, one per task

<svg viewBox="0 -34 820 234" width="100%" style="max-width:820px" role="img"
     aria-label="Task state machine: pending to ready via refresh_ready, and ready back to pending via depends when a new unresolved dependency is added. Ready to assigned via assign, assigned to running via claim, running to done via complete. Abandon or reclaim return assigned or running to pending. Any non-terminal state can be cancelled."
     xmlns="http://www.w3.org/2000/svg" fill="none" stroke="currentColor">
  <defs>
    <marker id="qp-arrow" viewBox="0 0 10 10" refX="9" refY="5"
            markerWidth="6" markerHeight="6" orient="auto-start-reverse">
      <path d="M0,0 L10,5 L0,10 z" fill="currentColor" stroke="none"/>
    </marker>
  </defs>
  <g font-family="ui-sans-serif,system-ui,sans-serif" font-size="13"
     fill="currentColor" stroke="none" text-anchor="middle">
    <text x="52"  y="58">pending</text>
    <text x="207" y="58">ready</text>
    <text x="367" y="58">assigned</text>
    <text x="533" y="58">running</text>
    <text x="688" y="58">done</text>
    <text x="610" y="177">cancelled</text>
    <g font-size="11" opacity="0.85">
      <text x="132" y="30">refresh_ready</text>
      <text x="282" y="30">assign</text>
      <text x="452" y="30">claim</text>
      <text x="614" y="30">complete</text>
      <text x="130" y="-18">depends (new unresolved dep)</text>
      <text x="300" y="136">abandon / reclaim</text>
      <text x="345" y="165">cancel</text>
    </g>
    <text x="14" y="177" text-anchor="start" font-size="11" opacity="0.85">any non-terminal</text>
  </g>
  <g stroke-width="1.2">
    <rect x="10"  y="36" width="84" height="34" rx="5"/>
    <rect x="170" y="36" width="74" height="34" rx="5"/>
    <rect x="320" y="36" width="94" height="34" rx="5"/>
    <rect x="490" y="36" width="86" height="34" rx="5"/>
    <rect x="652" y="36" width="72" height="34" rx="5" stroke-width="2.2"/>
    <rect x="568" y="155" width="84" height="34" rx="5" stroke-width="2.2"/>
  </g>
  <g stroke-width="1.2" marker-end="url(#qp-arrow)">
    <path d="M94,53 H164"/>
    <path d="M244,53 H314"/>
    <path d="M414,53 H484"/>
    <path d="M576,53 H646"/>
    <path d="M207,36 V-4 H52 V30"/>
    <path d="M533,70 V118 H52 V76"/>
    <path d="M130,172 H562" stroke-dasharray="5 4"/>
  </g>
  <g stroke-width="1.2">
    <path d="M367,70 V118"/>
  </g>
</svg>

<details>
<summary>Same diagram as text (for viewers that strip inline SVG)</summary>

```text
        depends (new unresolved dep)
     ┌──────────┐
     ▼          │  assign            claim           complete
  pending ──▶ ready ──────▶ assigned ──────▶ running ──────────▶ done
     ▲                           │               │
     │  refresh_ready            └───────────────┘
     │  (deps resolved)            abandon / reclaim
     └─────────────────────────────────┘

  any non-terminal ──cancel──▶ cancelled
```

</details>

Terminal states are `done` and `cancelled`. Everything else is in flight.

**Every mutating command is exactly one edge in this graph.** That is why those
files are small — an edge is a small thing. [`assign.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/assign.rs)
is `ready → assigned`; [`claim.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/claim.rs) is
`assigned → running`.

Release paths (`abandon`, `reclaim`) both return the task to `pending` rather
than guessing whether its dependencies still hold. `refresh_ready` then promotes
it to `ready` when they do. One rule, one place.

### 2. A DAG that decides `pending` vs `ready`

The `dep` table. A task is `ready` when no dependency is left in a non-terminal
state.

Promotion has exactly one implementation: `refresh_ready` in
[`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs), which any command that might resolve a dependency
calls. The reverse edge is separate — adding an unresolved dependency to a
`ready` task demotes it back to `pending`, and that guarded `UPDATE` lives in
[`depends.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/depends.rs). The unresolved-dependency predicate
itself is therefore written in more than one place; `store.rs` documents the
read-side copy. Adding a terminal state means updating each of them.

Cycle prevention lives in `would_cycle`, a recursive CTE in the same file.

### 3. An append-only event log

Every mutation writes a row to `event` via `insert_event`. This is the audit and
forensics spine: [`timeline`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/timeline.rs),
[`decisions`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/decisions.rs), and [`watch`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/watch.rs) are
all just queries over it.

Events are only ever inserted inside the same transaction as the mutation they
describe. Because writers serialize (see below), `event.id` is gap-free — which
is what makes `watch` correct.

## The invariant that repeats everywhere

This is the heart of the system. Read it once and ~13 files become the same file.

```rust,ignore
db::with_tx(&mut conn, |tx| {                       // 1. BEGIN IMMEDIATE — take the write lock now
    let n = tx.execute(
        "UPDATE task SET state = ?1 WHERE id = ?2 AND state = ?3",   // 2. guarded edge
        rusqlite::params![db::State::Assigned, task_id, db::State::Ready])?;
    if n != 1 {
        return Err(db::conflict("not_ready", "...", Some(display_id)));  // 3. exactly-one check
    }
    db::insert_event(tx, ..., "state_change", ...)?;                 // 4. audit trail
    Ok(())
})?;
```

State values are bound as `db::State` rather than spelled as SQL string
literals, so the CLI vocabulary and the transition vocabulary come from one
definition. Multi-state predicates (`WHERE state IN (...)`) stay literal —
they do not parametrise idiomatically in rusqlite.

Four parts, and each one is load-bearing:

1. **`BEGIN IMMEDIATE`** (`with_tx` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs)) takes SQLite's
   write lock at transaction start rather than at first write. Writers therefore
   serialize, and no other process can interleave between a read and a write
   inside the block.
2. **The guarded `WHERE`** means the transition only applies if the task is
   still where the caller thought it was.
3. **`n != 1`** means you *find out* when it wasn't, instead of silently
   clobbering. This is what makes a claim atomic: two agents racing the same
   task produce exactly one winner and one `conflict` error. The error
   constructors — `conflict`, `not_owner`, `not_found`, `invariant`,
   `invalid_input` — are the agent-facing taxonomy: the variant says whether
   to retry, give up, or escalate, and `conflict` carries a stable code
   string such as `not_ready` or `already_claimed`.
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

Projections cannot corrupt anything, and the risk asymmetry is stark.
[`report.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/report.rs) is among the largest command files and
also the least dangerous — worst case it emits a wrong number. Meanwhile
[`claim.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/claim.rs) is one of the smallest, and if it is wrong
you double-dispatch work to two agents.

Scrutiny should follow risk, not line count.

## Layering

```text
main.rs      clap subcommand enum → dispatch. Routing only.
  db.rs      connection, PRAGMAs, migrations, transactions, error taxonomy,
             guarded-transition helpers
  store.rs   canonical read queries + the row types they return
  outcome.rs the Outcome trait and emit — one success payload per mutator,
             rendered as either JSON or a prose line
  cmd/*.rs   argument parsing and rendering
```

`outcome.rs` is why `--json` is not a second code path: a mutator builds one
struct and `emit` chooses the representation, so the two output modes cannot
drift apart.

[`store.rs`](https://github.com/FMazzoni/quipu/blob/main/src/store.rs) exists because the same read queries were
hand-written across many command files in subtly divergent forms. Its own module
header explains the scope rules — notably that **guarded-transition `UPDATE`s
deliberately do not move there**, since each has a distinct `WHERE`/`SET` and
relocating the highest-stakes code for tidiness would be a bad trade.

This layering is **in progress**, not finished. `store.rs` is being populated
incrementally, and `cmd/*.rs` files still contain SQL that has not migrated yet.

## Storage

One SQLite file, default `.quipu/db.sqlite`. `resolve_path` in
[`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) picks it in three tiers:

1. An explicit `--db` / `QP_DB`, which wins outright.
2. Otherwise the nearest `.quipu/db.sqlite` walking up from the cwd.
3. Otherwise the one beside the repo root from `git rev-parse --git-common-dir`.

Tier 3 exists for worktrees: a `wt`-managed worktree is a *sibling* of the main
checkout, not a child, so walking up ancestors never reaches its `.quipu/`. This
is what lets a subagent run bare `qp` with no environment set up.

Each store stamps a `project_uuid` at `qp init`. Passing `--db`/`QP_DB`
explicitly while the cwd would have resolved to a *different* store emits a
mismatch warning — the guard against filing into the wrong project. Because that
only fires under an explicit path, its audience is automation, which is why under
`--json` it is emitted as JSON rather than prose (stderr is JSON Lines: warnings,
then at most one error).

PRAGMAs set on every open, in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs):

| pragma | value | why |
|---|---|---|
| `journal_mode` | `WAL` | readers don't block the writer |
| `synchronous` | `NORMAL` | safe under WAL, much faster than `FULL` |
| `foreign_keys` | `ON` | referential integrity is enforced, not assumed |
| `busy_timeout` | `5000` | wait for a contended write lock instead of failing |

Tables: `meta`, `default_tag`, `task`, `dep`, `assignment`, `event`, `tag`,
`relation`. See [`schema.sql`](https://github.com/FMazzoni/quipu/blob/main/src/schema.sql), which is the authority.

Two notes on the schema as it stands:

- `task.state` is bare `TEXT`: **there is no `CHECK` constraint**, so the domain
  is not enforced by the database itself. That is deliberate — adding one would
  require SQLite's full table-rebuild dance, and no other `TEXT` column carries a
  `CHECK` either. Enforcement is Rust-side instead: `db::State` is the single
  definition, bound as a parameter in every transition and derived as a
  `clap::ValueEnum` so `--state` rejects a typo at parse time rather than
  silently matching zero rows.
- `created_at` and friends default to SQLite's `strftime`, while Rust-side
  timestamps come from `now_rfc3339` in [`time.rs`](https://github.com/FMazzoni/quipu/blob/main/src/time.rs). These
  produce different sub-second precision, so cross-table lexicographic time
  comparison is not reliable.

## Identifiers

Tasks carry a rowid and a display id (`QP-1`), formatted by
[`id.rs`](https://github.com/FMazzoni/quipu/blob/main/src/id.rs). The prefix is per-store, fixed at `qp init`, default
`QP`.

`resolve_full` parses the reference and matches on the **numeric rowid**, not on
the display-id string. So the prefix and any zero padding in user input are
informational: `QP-1`, `QP-001`, and `qp-1` all reach the same row. It returns
both the rowid and the canonical `display_id`, which is what mutating commands
echo — never the raw argument the user typed.

## Exit codes

Agents branch on these, so they are a contract. They come from two places:
`QuipuError::exit_code` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) maps the error taxonomy to
0/1/2, and [`main.rs`](https://github.com/FMazzoni/quipu/blob/main/src/main.rs) applies it; `wait` exits 3 and 4
directly.

| code | meaning |
|---|---|
| 0 | success |
| 1 | `invalid_input`, or any error outside the taxonomy |
| 2 | `conflict`, `not_owner`, `not_found`, `invariant` — the state of the store refused the operation |
| 3 | `wait` timed out |
| 4 | `wait --cohort-done` matched an empty cohort |

Code 2 is the interesting one: it is the *expected* outcome of losing a race, not
a failure.

Two outcomes fall outside that contract and will surprise you:

| code | when | status |
|---|---|---|
| 2 | clap's own argument parsing failed — unknown flag, missing required arg. *Not* a store conflict | clap's default, not ours. Tell them apart by the `error: unexpected argument` prose and the absence of a `{"error": ...}` envelope under `--json` |
| 101 | Rust panic on **broken pipe** — stdout closed while `qp` was still writing | unfixed; tracked as QP-139 |

101 is the trap. `qp show QP-1 | head -1` exits 101 with
`failed printing to stdout: Broken pipe (os error 32)` on stderr. It means the
reader went away, not that `qp` crashed or that the store is damaged. It only
fires when the output outruns the pipe buffer, so short output pipes cleanly and
the bug reads as intermittent.

## Barriers

`qp wait --cohort-done --tag <t>` blocks until the tag-matched cohort has
`total > 0` and no task left in a non-terminal state. The empty-cohort case is a
distinct exit code (4) rather than a silent success, because "nothing matched"
and "everything finished" must not look alike to an orchestrator.

## Symptom index

Everything above is organised by structure. Debugging needs the inverse index.
Nobody starts from "what does `store.rs` do" — they start from "this task is
stuck in `pending` and I do not know why". This section is that lookup. Every row
is a fact about the current code, reproducible from the CLI; none of it is
explanation.

| symptom | diagnose with | cause |
|---|---|---|
| stuck in `pending` | `qp tree <id>` | an upstream dep is not `done`. `refresh_ready` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) owns `pending`→`ready`, but readiness is **not single-homed**: [`depends.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/depends.rs) also demotes `ready`→`pending` when a new edge lands, so a task can leave `ready` without anything having acted on it directly |
| exit 2, code `already_claimed` | `qp timeline <id>` | someone claimed it first. **This is the expected outcome of losing a race, not a failure** — the timeline names the winner. Do not treat it as an error path |
| exit 2, kind `not_owner` | `qp show <id>` | a different agent holds the open assignment; the envelope's `owner` field names them. Do not retry — escalate, or `qp reclaim` |
| exit 2, code `not_ready` | `qp show <id>` | `assign` requires state `ready`; the task is `pending`, `assigned`, `running`, or terminal |
| exit 2, code `no_open_assignment` | `qp show <id>` | `claim`/`complete`/`abandon` found no un-completed assignment row — an `assign` that never happened, or a prior `abandon`/`reclaim` that already closed it |
| exit 2, code `not_claimed` | `qp timeline <id>` | `complete` on a task that was assigned but never claimed; the `assigned`→`running` edge was skipped |
| exit 2, code `not_assigned_or_running` | `qp show <id>` | `abandon`/`reclaim` on a task that is not in flight |
| exit 2, code `not_blockable` | `qp show <id>` | `block` requires `assigned` or `running`; the message carries the actual state |
| exit 2, code `already_terminal` | `qp show <id>` | `cancel` on something already `done` or `cancelled` |
| exit 2, code `not_editable` | `qp show <id>` | `edit` on a terminal task, or the row vanished |
| exit 2, kind `invariant`, code `dependency_cycle` | `qp tree <id>` | the edge would close a loop; `would_cycle` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) rejects it before the insert. Replan — retrying cannot help |
| exit 2, kind `not_found` | `qp list` | no such task or edge. Raised by `resolve_full` in [`id.rs`](https://github.com/FMazzoni/quipu/blob/main/src/id.rs) |
| barrier released immediately, work unfinished | `qp wave` | `qp wait --state running --empty` releases on any *gap* — including before the first agent has started. Use `qp wait --cohort-done --tag <t>`, which additionally requires `total > 0` |
| barrier exits 4 | `qp list --tag <t>` | `--cohort-done` matched zero tasks: wrong tag, or nothing tagged yet |
| barrier exits 3 | `qp wave` | `--timeout-secs` elapsed with the cohort still unfinished. A timeout, not a conflict |
| an expected event is absent from `qp timeline` | `qp timeline <id>` | `insert_event` runs *inside* the same transaction as the mutation, so a failed mutation writes no event at all. The timeline is byte-identical before and after an exit-2 command — absence of an event is positive evidence the transition never landed |
| exit 101 mid-pipeline | rerun without the pipe | broken pipe; see the exit-codes section above. QP-139 |
| `{"warning": {"kind": "project_uuid_mismatch"}}` | `qp status` | `--db`/`QP_DB` points at a different store than the working directory would have resolved to. See the store-discovery tiers in `README.md` |

The error-envelope shape per `kind` — which fields are actually present, and why
`code` is not one of them on `not_owner` — is tabulated in `README.md`. Branch on
`kind` before reading any other field.

### The introspection commands, by question

| question | command |
|---|---|
| what state is this in, and who holds it | `qp show <id>` |
| what happened to it, in order | `qp timeline <id>` |
| why is it not ready | `qp tree <id>` |
| what have agents decided, project-wide | `qp decisions` |
| what is in flight right now | `qp wave` |
| aggregate counts by state | `qp status` |
| filter by tag or state | `qp list` |
| block until something changes | `qp watch` |

### One code you cannot reach

`stale_open_assignment` in [`assign.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/assign.rs) is defensive
only. It fires when a `ready` task already carries an open assignment, which no
CLI sequence produces: the guarded `UPDATE` to `assigned` runs first, and a
`ready` task should never hold one. If you ever see it, the store is in a state
the state machine calls impossible — that is a bug report, not a usage error.
See QP-137 / QP-142.

## A reading path

To understand the system, read along the state machine rather than the file
tree — the alphabetical `cmd/` listing carries no signal.

1. `with_tx` and `insert_event` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) — the two primitives
   everything else uses.
2. [`add.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/add.rs) → [`assign.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/assign.rs) →
   [`claim.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/claim.rs) → [`complete.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/complete.rs),
   in that order. That is one task's whole life.
3. [`abandon.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/abandon.rs) and
   [`reclaim.rs`](https://github.com/FMazzoni/quipu/blob/main/src/cmd/reclaim.rs) — the recovery edges.
4. `refresh_ready` and `would_cycle` in [`db.rs`](https://github.com/FMazzoni/quipu/blob/main/src/db.rs) — the two DAG
   operations.

Skip everything else until you need it.

[`tests/cli.rs`](https://github.com/FMazzoni/quipu/blob/main/tests/cli.rs) is also worth reading early: the tests drive
the real binary and assert on its observable behaviour, so they read closer to
shell scripts than to Rust, and they are the executable specification for
everything above.
