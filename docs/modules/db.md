This module also holds the error types and the shared mutation utilities. Every
state mutation in the crate routes through `with_tx` + a guarded conditional
UPDATE — see `decisions/guarded-state-transitions.md` in the vault for the
contract.

## Single-implementation rules

Each item below has exactly one implementation, because a second copy is a
correctness bug rather than a duplication smell.

- **`with_tx`** opens `BEGIN IMMEDIATE`, taking the write lock at the top of the
  transaction rather than at the first write. Every mutator goes through it.
- **`refresh_ready`** is the only implementation of `pending` → `ready`. Any
  command that might resolve a dependency calls it. Two copies of the readiness
  rule is how a release path and a completion path come to disagree about what
  "ready" means.
- **`would_cycle`** is the only cycle check, a recursive CTE that must run inside
  the caller's transaction so it can see edges just inserted.
- **`State`** is one enum serving both the SQL `state` column and clap's
  `--state` vocabulary, so an invalid `--state` is rejected at parse time rather
  than matching zero rows.
- **`insert_event`** is the only writer of the audit log, and only ever runs
  inside a transaction. That is what makes `event.id` gap-free to readers and
  `watch`'s `WHERE id > last_seen` correct.

## Error taxonomy

`QuipuError` has five variants, split by *what a caller should do next*:
`conflict` (lost a race — may retry), `not_owner` (never retry), `not_found`,
`invariant` (structural, e.g. a cycle), and `invalid_input`. The variant drives
the exit code, which is what an orchestrating skill branches on. The `code`
string inside `conflict` and `invariant` is a finer label for logs, and grows
additively — a new failure reason does not require a new exit code.

The `--json` envelope carries a sixth `kind` the enum does not: `internal`, which
`main.rs` emits for any error that does not downcast to a `QuipuError`, with exit
1. Five variants, six kinds. Read the list from `QuipuError::kind` plus that
fallback, not from the variant list alone; `README.md`'s envelope table is the
full six.

## Migration on open

`open()` migrates, and every command but `init` goes through it. A binary
upgraded before `qp init` is re-run depends on the stale schema self-healing
rather than failing with "no such column"
(`read_command_self_heals_stale_schema_without_init`). A matching
`schema_version` skips the DDL re-apply, which is worth 1–3 ms per invocation
against the cold-start budget.

That skip is the trap. **Adding DDL to `schema.sql` without bumping
`SCHEMA_VERSION` ships it to fresh stores only.** Existing stores already stamp
the matching version, so `migrate` skips `execute_batch(SCHEMA)` and the new
table or index never appears; because the DDL is all `IF NOT EXISTS`, nothing
errors and nothing warns. Test any schema change against a copy of a real store —
a fresh-store test passes either way.

`idx_assign_one_open` (QP-142) is the worked example: a partial unique index on
`assignment(task_id) WHERE completed_at IS NULL`, enforcing the
one-open-assignment-per-task invariant that `cmd/assign.rs` previously held by
convention. It needed the `SCHEMA_VERSION` 2 → 3 bump to reach existing stores.

A partial unique index added to a store that already violates it fails at
`CREATE` time, and under this mechanism that means every subsequent `qp`
invocation errors — the store is bricked, not degraded. Query the real store for
violations before adding one.

## Path resolution

Three tiers, and the third is the surprising one: an explicit `--db`/`QP_DB`
wins; otherwise the nearest `.quipu/` walking up from the cwd; otherwise the one
beside the repo root from `git rev-parse --git-common-dir`. That last tier exists
for worktrees, which are *siblings* of the main checkout rather than children, so
walking up ancestors never reaches the store. It is what lets a subagent in a
worktree run bare `qp` with no environment set up
(`resolve_path_finds_store_from_worktree`); removing it would push every worktree
agent into creating its own empty database.

The project-uuid mismatch warning fires only under an explicit path — that is,
only in automation — which is why it respects `--json`. Under `--json`, stderr is
JSON Lines, and a prose warning would leave the consumer unable to parse the
error that might follow it.
