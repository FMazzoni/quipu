Target layering (see the audit-remediation plan in `$QUIPU_VAULT/plans/`):

```text
db.rs      — connection, transactions, migrations, guarded-transition helpers
store.rs   — canonical read queries + the row types they return
cmd/*.rs   — argument parsing and rendering only, no SQL
```

**The last line is the goal, not the current state.** Several command modules
still hand-roll `SELECT`s that have not migrated here. Extraction is incremental
by design; treat an un-migrated query as unfinished work rather than as a
counter-example to the rule.

## Purpose

Three query families were hand-written across command modules in divergent
forms before this extraction: the "latest agent" lookup (`list.rs`, `wave.rs`),
the unresolved-dep predicate, and the `event LEFT JOIN task` tail
(`timeline.rs`, `watch.rs`, `decisions.rs`, which numbered their `?N`
placeholders differently). They are now `LATEST_AGENT_SUBQUERY`,
`unresolved_blockers_by_task`, and `events` + `EventFilter`.

Divergence is the risk, not verbosity. Adding a terminal state means updating
every copy correctly, and missing one is a silent logic bug.

## Scope discipline

Deliberate, from the QP-68 research:

- **Read queries and their row types belong here.**
- **Guarded-transition `UPDATE`s do not.** They are not duplicated with each
  other — each has a distinct `WHERE`/`SET` — so moving them would relocate the
  highest-stakes code in the project for taxonomic tidiness alone.
- **Rendering helpers do not.** `wrap_text` (in `show.rs`) does no database work,
  and markdown/HTML rendering has moved out of the binary entirely into the
  `report-render` skill.

## The unresolved-dep predicate

`t2.state NOT IN ('done','cancelled')` — "this dependency is not yet resolved" —
is written three times and stays that way (QP-146, closed won't-do). Two of the
three are guarded-transition `UPDATE`s, which the scope discipline above
excludes on purpose; consolidating the one read-side copy while the other two
stay put buys tidiness in exchange for a partial abstraction that hides where
the real risk is.

**The risk is divergence, not duplication.** Adding a terminal state — or
renaming one — means updating all three correctly, and missing one is a silent
logic bug: a task that never promotes to `ready`, or one that promotes while
still blocked. So the deliverable is findability. All three sites:

| Site | Kind | What it does |
| --- | --- | --- |
| `db::refresh_ready` (`src/db.rs`) | guarded `UPDATE` | promotes `pending` → `ready` when no dep is unresolved |
| `cmd::depends` demotion `UPDATE` (`src/cmd/depends.rs`) | guarded `UPDATE` | demotes `ready` → `pending` when a newly added dep is unresolved |
| `store::unresolved_blockers_by_task` (`src/store.rs`) | read query | lists the unresolved blockers per task, for rendering |

`src/cmd/depends.rs` also runs the predicate in a plain `SELECT` just above its
demotion `UPDATE`, snapshotting promotion candidates before calling
`db::refresh_ready`. It changes together with that module's `UPDATE`, so it is
not a fourth independent site — but it is a fourth textual occurrence, and a
sweep that misses it will still compile.

Search for the literal `NOT IN ('done','cancelled')`. It is spelled identically
at every site, with no whitespace variation, which is worth preserving for
exactly that reason. The search returns seven SQL occurrences, not four: the
other three — `cmd/cancel.rs`, `cmd/edit.rs`, `cmd/wait.rs` — apply the same
literal to the task's *own* state as a terminal-state guard, not to its
dependencies. Renaming a terminal state touches all seven; the reasoning above
about promotion and demotion applies only to the four dep-side ones.

**What would reopen this:** a fourth independent site appearing, a terminal
state actually being added and one of the three being missed in the process, or
the guarded-`UPDATE` exclusion itself being revisited. Any of those turns three
hand-written copies from a defensible call into an accident waiting to repeat.
