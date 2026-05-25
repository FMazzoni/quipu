---
name: wave-orchestrate
description: Run a wave cycle for quipu — plan, dispatch parallel subagents in worktrees, merge, optionally critique, wrap. The coordinator's playbook.
allowed-tools: Read Glob Grep Bash Agent Edit Write
---

# Wave Orchestrate

> Run a wave cycle (plan → dispatch → merge → optionally critique → wrap). You are the coordinator; never edit code.

You are the **coordinator**. Subagents do all code changes inside `wt`-managed worktrees. Your job is phasing, dispatch, merging conflicts, and wrap-up.

## Hard rules (read before phasing)

- [ ] Never edit code. Exception: resolve merge conflicts during `wt merge` rebase.
- [ ] Never use `isolation: "worktree"` on Agent calls. Always `wt switch -c`.
- [ ] Never run `cargo test` (workspace-wide) while subagents are active — multiple rustc graphs OOM the machine. Run the single full test pass at wrap-up.
- [ ] No Co-Authored-By trailer, no "Generated with Claude Code" footer on commits.
- [ ] All other hard rules: see `CLAUDE.md` (leanness, no async, no tracing, guarded state transitions).

## Phase 0 — Pre-scaffold (when needed)

When parallel slices will all touch `src/cmd/mod.rs` and `src/main.rs` (i.e. each slice adds a new subcommand), land the structural shape on `main` *first*, in a single coordinator-dispatched scaffold commit. This eliminates the rote add/add conflict pattern.

Scaffold commit contains:
- empty stub modules (`src/cmd/<new>.rs` with `pub fn run(_a: Args) -> Result<()> { unimplemented!() }`)
- `mod <new>;` lines in `src/cmd/mod.rs`
- clap variants + match arms in `src/main.rs`
- help-test golden file updates if applicable

Commit message: `chore: scaffold <wave-name> surface (stub <cmd>, <cmd>)`.

See: commits `5f88113` (Pattern C) and `2806c30` (Pattern D) for shipped examples.

## Phase 1 — Plan

1. **Research subagent** (sonnet/haiku). Have it read relevant files + existing patterns and report: file paths, line ranges, key types/functions, integration points.
2. **Write the plan yourself** at `docs/superpowers/plans/YYYY-MM-DD-<feature>.md`. The plan must contain **actual code** in every step — no "TBD", no "implement X appropriately".

Plan structure:

```markdown
# <Feature> Plan

**Goal:** <one sentence>
**Architecture:** <2-3 sentences>

## Locked decisions
- <pre-decided constraints — these are off-limits to critic dispute>

## Slice A — <name> (independent)
### Task A1: <component>
**Files:** modify `path:line-range`
**Steps:**
- [ ] <step with concrete code>
- [ ] narrow test: `cargo test --test cli -- <filter>`
- [ ] commit: `feat(cmd): ...`

## Slice B — <name> (independent)
...

## Integration (sequential, after merge)
...
```

**Plan-scope discipline:** scope each ticket to the cleanest edit boundary (one cohesive concern), not the minimum file count. A slice that touches 4 files for one concept is fine; a slice that touches 1 file but covers 3 concepts is not.

## Phase 2 — Ticket (when multi-subagent)

For multi-subagent waves: open a wave ticket and child impl tickets so `qp tree`, `qp wave`, and `qp timeline` reflect the DAG.

```bash
./target/release/qp add "Wave: <feature>" --tag kind:wave
./target/release/qp add "<Slice A title>" --tag kind:impl
./target/release/qp add "<Slice B title>" --tag kind:impl
./target/release/qp depends QP-<wave> QP-<a> QP-<b>
```

**Skip ticketing** for single-subagent waves — open the impl ticket directly, no wave wrapper.

## Phase 3 — Dispatch

```bash
wt switch -c wu-<slug-a> --no-cd --no-verify -y
wt switch -c wu-<slug-b> --no-cd --no-verify -y
wt list --full   # capture exact worktree paths
```

Then dispatch one `Agent` per slice **in a single message** for true parallelism. Embed the slice body inline — never tell a subagent to read `.tmp/QP-N.md` or the plan file. The prompt **is** the contract.

### Subagent prompt template

```
You are implementing <slice title> for quipu, following the qp-implement
skill at .claude/skills/qp-implement/SKILL.md. Read that skill first.

**Working directory:** <ABSOLUTE PATH to wt-managed worktree>
cd there first. All commands and paths are relative to it.

**Ticket:** QP-<N>
**Agent id:** <slug-a>

## Slice body (embedded — do not search for a plan file)

<paste the slice's tasks + steps + code verbatim from the plan>

## Context

<files this touches, sibling-slice APIs you may reference, key types>

## Rules

- Bare `./target/release/qp` works from this worktree (git-common-dir
  fallback finds the main repo's .quipu/). Do NOT set QP_DB.
- Narrow tests only: `cargo test --test cli -- <filter>` or a specific
  test file. NEVER `cargo test` (no filter) while other agents are running.
- Single commit, conventional style, no Co-Authored-By trailer.
- All hard rules in CLAUDE.md apply (leanness, no async, no tracing,
  guarded state transitions, no db::now()).

## Required final steps (in order)

1. `./target/release/qp log QP-<N> decision "<one-sentence friction note>" --auto`
2. `./target/release/qp complete QP-<N> --as <slug-a>`

## Report shape

- **Status:** DONE | DONE_WITH_CONCERNS | BLOCKED | NEEDS_CONTEXT
- Per-task summary
- Narrow test output (paste)
- Friction note (one sentence — what was unobvious)
- Files changed
- Sibling-slice APIs you referenced (so coordinator knows merge order)
```

**Model selection:**

| Task                                            | Model            |
|-------------------------------------------------|------------------|
| Research / Explore                              | sonnet or haiku |
| Mechanical impl (1–2 files, clear spec)         | sonnet           |
| Integration / multi-file / judgment             | opus (default)   |
| Critic reviewers                                | sonnet           |
| Fix agents                                      | sonnet (opus for complex) |

### Handling results

- **DONE_WITH_CONCERNS:** read concerns; if correctness, fix before merge.
- **NEEDS_CONTEXT:** SendMessage to same agent with the missing piece.
- **BLOCKED:** assess (model? scope? plan gap?) — re-dispatch or escalate.

## Phase 4 — Merge

```bash
wt merge -C <worktree-path> -y && \
  ./target/release/qp tag QP-<N> add commit:$(git rev-parse --short=6 HEAD)
```

Chain the commit-tag in the same Bash call so it can't be forgotten. The tag uses the namespace `commit:<sha>` so reverse lookup is just `qp list --tag commit:<sha>` — no new commands needed.

For coordinator-direct commits (justfile edits, reactive fixes, doc-only work) that bypass the wave-orchestrate flow: still ticket them. Open a `qp add` retroactively if needed, then `qp tag` with the SHA. The system-of-record stays complete.

Merge order: foundational slice first (data model, types); feature slices that build on it second. If slice B references slice A's APIs, merge A first.

Conflicts on shared files (`src/cmd/mod.rs`, `src/main.rs`) are expected unless Phase 0 pre-scaffolding ran. Resolve manually — keep both halves is almost always right.

After merge: `wt list` should be clean. Leftover worktrees → `wt remove -C <path>`.

## Phase 5 — Critique (optional, default OFF)

**Skip critic for:**
- dead-code cleanup, mechanical renames
- single-flag additions, well-spec'd bug fixes
- changes < ~100 LoC with no new public surface

**Run critic when:**
- new state transitions / new guarded UPDATEs
- new public CLI commands or API surface
- architecture or schema changes
- > ~100 LoC

Dispatch ≤4 critic agents in parallel, one lens each. Reference `.claude/skills/qp-critique/SKILL.md` in the prompt.

| Lens             | Focus                                                   |
|------------------|---------------------------------------------------------|
| Correctness      | bugs, panics, edge cases, off-by-ones                   |
| Architecture     | module boundaries, coupling, state model                |
| Spec compliance  | plan vs implementation divergence                       |
| UX / CLI         | flag names, help text, error messages                   |
| Performance      | allocations, hot-path cost                              |
| API surface      | naming, forward-compat of new public surface            |

## Phase 6 — Fix

**Auto mode:** act only on Critical findings. Important/Minor/Observation get filed to `docs/bugs/YYYY-MM-DD-<short>.md`.

**Interactive mode:** triage all findings with the user, then dispatch fix subagents in parallel (one worktree per topic-affinity group via `wt switch -c fix-<slug>`). After merge, mark addressed findings `**Status: FIXED in <sha>**` in the critic file.

## Phase 7 — Wrap

1. `cargo test` once, in foreground, after all merges and with no agents running:
   ```bash
   cargo test 2>&1 | grep "^test result"
   ```
2. Leanness gates: stripped-binary size, `qp --version` cold start, RSS. Confirm under budget (CLAUDE.md).
3. Update `docs/TODO.md` — check off completed items.
4. Update `docs/DECISIONS.md` if architectural decisions were made.
5. Update `docs/HANDOFF.md` — append session entry (built / decisions / critic count / next).
6. Vault notes for any new decision: `$QUIPU_VAULT/decisions/`.
7. File deferred bugs to `docs/bugs/`.
8. Report to user: commit range, test count, deferred items.
