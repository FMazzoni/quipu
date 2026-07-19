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

## Why this module exists

The same queries were hand-written across many command files in subtly divergent
forms — the "latest agent" lookup existed in 3 shapes across 11 sites, the
unresolved-dep predicate in 9, the event-tail `SELECT` in 3 column shapes across
6.

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
