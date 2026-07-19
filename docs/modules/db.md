Also holds the error types and the shared mutation utilities. Every state
mutation in the crate routes through `with_tx` + a guarded conditional
UPDATE — see `decisions/guarded-state-transitions.md` in the vault for the
contract.

## What is here because it must have exactly one implementation

The organising principle is not "database things" — it is "rules that would be
dangerous to have two copies of".

- **`with_tx`** opens `BEGIN IMMEDIATE`, so the write lock is taken at the top of
  the transaction rather than at the first write. Every mutator goes through it,
  which is what makes "no other process interleaved between my read and my write"
  a property of the codebase rather than of each author's care.
- **`refresh_ready`** is the only implementation of `pending` → `ready`. Any
  command that might resolve a dependency calls it. A second copy of the
  readiness rule is how a release path and a completion path come to disagree
  about what "ready" means.
- **`would_cycle`** is the only cycle check, a recursive CTE that must run inside
  the caller's transaction so it can see edges just inserted.
- **`State`** is one enum serving both the SQL `state` column and clap's
  `--state` vocabulary, so the CLI surface and the transition surface cannot
  drift. This is why an invalid `--state` is rejected at parse time rather than
  matching zero rows.
- **`insert_event`** is the only writer of the audit log, and it is only ever
  called inside a transaction. That is what makes `event.id` gap-free to readers
  and `watch`'s `WHERE id > last_seen` correct.

## The error taxonomy is an agent-facing contract

Four variants, not one per failure site, and the split is by *what a caller
should do next*: `conflict` (you lost a race — maybe retry), `not_owner` (the
task is someone else's — never retry), `not_found`, `invariant` (structural, e.g.
a cycle), and `invalid_input`. The variant drives the exit code, which is what an
orchestrating skill actually branches on. The `code` string inside `conflict` and
`invariant` is a finer label for logs and precise skill authoring, and can grow
additively without breaking existing matchers — so adding a new failure reason
does not require a new exit code.

## Two behaviours that look wrong and are not

**`open()` migrates.** Every command but `init` goes through it, and it must stay
migration-capable: someone who upgrades the binary and then runs a read command
before ever running `qp init` depends on the stale schema self-healing rather
than surfacing as a confusing "no such column"
(`read_command_self_heals_stale_schema_without_init`). The version stamp is what
keeps that cheap — a matching `schema_version` skips the DDL re-apply entirely,
which is worth 1–3 ms on every single invocation and therefore worth having on a
tool with a cold-start budget.

**Path resolution has three tiers, and the third is the surprising one.** An
explicit `--db`/`QP_DB` wins; otherwise the nearest `.quipu/` walking up from the
cwd; otherwise the one beside the repo root from `git rev-parse --git-common-dir`.
That last tier exists entirely for worktrees, which are *siblings* of the main
checkout rather than children — so walking up ancestors never reaches the store.
It is what lets a subagent in a worktree run bare `qp` with no environment set up
(`resolve_path_finds_store_from_worktree`), and removing it would silently push
every worktree agent into creating its own empty database.

The project-uuid mismatch warning fires only under an explicit path — that is,
only in automation — which is why it respects `--json`. Under `--json`, stderr is
JSON Lines, and a prose warning would leave the consumer unable to parse the
error that might follow it.
