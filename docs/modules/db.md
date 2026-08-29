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

`QuipuError` has six variants, split by *what a caller should do next*:
`conflict` (lost a race — may retry), `not_owner` (never retry), `not_found`,
`invariant` (structural, e.g. a cycle), `invalid_input`, and `no_store`. The
variant drives the exit code, which is what an orchestrating skill branches on.
The `code` string inside `conflict` and `invariant` is a finer label for logs,
and grows additively — a new failure reason does not require a new exit code.

The `--json` envelope carries a seventh `kind` the enum does not: `internal`,
which `main.rs` emits for any error that does not downcast to a `QuipuError`,
with exit 1. Six variants, seven kinds. Read the list from `QuipuError::kind`
plus that fallback, not from the variant list alone; `README.md`'s envelope
table is the full seven.

## Resolution is not creation

`resolve_path` answers "where *would* the store be" — a question with an answer
even on a machine that has none. Whether that path may be *created* is a
separate decision, and only `qp init` gets to take it: `open` calls
`require_store` first, and `main.rs` calls `require_attached` ahead of dispatch
for every subcommand but `init` (and `install-skills`, which opens no store).

The two were the same act until this split, and the cost was that `qp` could not
distinguish an empty project from a misconfigured machine. `qp list --json` in a
directory with no store printed `[]` and exited 0, having quietly created
`./.quipu/db.sqlite` on the way; the next `qp add` filed a real ticket into it.
An agent handed the wrong working directory answered its own first instruction
with "no such task" and exit 2 — the code documented as *retry may succeed* —
so the failure read as a bad ticket id. `no_store` (exit 6) is the fix: it names
the directory, says what to run, and creates nothing.

`require_store` under `open` is a backstop rather than the primary check. The
primary check lives in `main.rs` because that is where the cwd-versus-`--db`
distinction is still known, and so the message can name the directory the
operator was actually standing in; the one under `open` exists so a new command
module cannot reintroduce create-on-demand by opening a path nobody made.

The exception that proves the rule is `--db`/`QP_DB` pointing at a file that is
not there. It fails the same way rather than creating: an explicit path is a
*claim about where the store is*, and honouring it by conjuring an empty one is
precisely the cross-machine silent failure this exists to prevent. `qp init`
with the same path is the way to mean "create it".

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

Four tiers, and the third is the surprising one: an explicit `--db`/`QP_DB`
wins; otherwise the nearest `.quipu/` walking up from the cwd *and stopping at
the checkout root*; otherwise the one beside the repo root from `git rev-parse
--git-common-dir`; otherwise the nearest `.quipu/` above the checkout root. The
git tier exists for worktrees, which are *siblings* of the main checkout rather
than children, so walking up ancestors never reaches the store. It is what lets a
subagent in a worktree run bare `qp` with no environment set up
(`resolve_path_finds_store_from_worktree`); removing it would push every worktree
agent into creating its own empty database.

The boundary on the walk is load-bearing, not decoration. Without it the walk
runs past the worktree to the filesystem root, so a `.quipu/` in any parent
directory wins and the git tier never gets a turn — and since that store is
valid, the wrong project is read and written with no error to notice
(`worktree_prefers_its_repo_store_over_an_ancestor_store`). Bounding rather than
simply putting git first is what keeps a store nested inside the worktree ahead
of the main repo's (`nested_store_inside_a_worktree_still_wins`). git is only
spawned when the checkout has no store of its own, so the ordinary case pays no
subprocess.

The mismatch warning compares against `resolve_path` itself rather than a second
private walk, so it describes the store a bare `qp` would really have used and
fires only on a real difference. The duplicated walk it replaced disagreed with
resolution inside a worktree and warned on *every* command for a consistent
`QP_DB` — and a warning that is always on for a legitimate setup is one people
learn to filter away rather than read.

The project-uuid mismatch warning fires only under an explicit path — that is,
only in automation — which is why it respects `--json`. Under `--json`, stderr is
JSON Lines, and a prose warning would leave the consumer unable to parse the
error that might follow it.
