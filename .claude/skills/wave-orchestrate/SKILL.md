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
- [ ] **Never push to `main`.** It is protected (ruleset `protect-main`): direct pushes are rejected server-side, a PR with `lint` green is required, no bypass. A wave lands on its own branch and reaches `main` through a PR you open at Phase 8; you confirm `lint` green and **stop** — the human merges. Name branches descriptively (no wave numbers or qp ids); fix branch/PR strategy at kickoff (see "Branch strategy").
- [ ] Never use `isolation: "worktree"` on Agent calls. Always `wt switch -c`.
- [ ] Never run workspace-wide `cargo test` while subagents are active — multiple rustc graphs OOM the machine. Run the single full test pass at wrap-up.
- [ ] **Never ask a subagent for a full-suite test count.** You own the suite total; they own their filters. See Phase 3.
- [ ] No Co-Authored-By trailer, no "Generated with Claude Code" footer on commits.
- [ ] All other hard rules: see `CLAUDE.md` (leanness, no async, no tracing, guarded state transitions).

## Branch strategy (decide once, at kickoff)

`main` is protected, so every wave reaches it through a PR — never a direct push. Before Phase 0, fix the branch/PR strategy for the **whole run**; do not revisit per wave:

- **If the invoking prompt names a strategy, obey it silently** — do not ask.
- **Otherwise, if a human is present, ask once** (then run unattended under the answer):
  - **one branch, one PR** — every wave lands on a single branch named for the campaign (`<slug>`); one PR merges the whole run to `main` at the end (Phase 8 runs once, after the final wave).
  - **staged PR per wave** — each wave gets its own descriptively-named branch and PR (Phase 8 runs per wave). Stack them: branch a wave off the previous wave's branch when it depends on that wave's unmerged work, off `main` when independent. Read the plan's slice dependencies to decide.
- **If fully autonomous, default to `staged PR per wave` and log it:** `./target/release/qp log <first-ticket> decision "branch strategy: staged PRs (default — no human at kickoff)" --auto`.

Name branches descriptively — `embeddings-search`, never `wave-3` or anything carrying a wave number or qp id. Those ids are instance-local: on a public repo they're visible and mean nothing to anyone else. The quipu↔branch link is the internal `branch:<name>` tag, not the branch name — at kickoff, once the branch is named, tag every ticket in the wave: `./target/release/qp tag <QP-N> add branch:<name>`. That tag is the durable ticket↔code pointer (see Phase 4); establish it here, once.

### The wave branch is a worktree; slices fan off it

The wave branch is the **integration worktree** — one branch, checked out in exactly one place (a branch cannot be checked out in two worktrees). Every slice is a *separate* worktree on its *own* branch, created with `--base` pointing at the wave branch, so each slice starts from the wave's actual current state. A worktree is based on a **commit**, and the wave branch's tip is just a commit — no patches, no snapshots; `--base` gives each slice the real, live state for free.

Create the wave's integration worktree off its base, before Phase 0:

```bash
wt switch -c <branch> --base <base>   # <branch> = the descriptive name; base = main, or the prior wave's branch when stacked
```

`--base` defaults to the default branch (`main`), so pass it explicitly whenever the base is a prior wave's branch. Every slice worktree (Phase 3) is created `--base <branch>`, and every slice merge (Phase 4) targets `<branch>` — never `main`, which is protected. It reaches `main` only through the Phase 8 PR.

## Phase 0 — Pre-scaffold (when needed)

When parallel slices will all touch `src/cmd/mod.rs` and `src/main.rs` (each slice adds a new subcommand), land the structural shape on **the wave branch** first (never `main`), in a single coordinator-dispatched scaffold commit. This eliminates the rote add/add conflict pattern.

Scaffold commit contains:
- empty stub modules (`src/cmd/<new>.rs` with `pub fn run(_a: Args) -> Result<()> { unimplemented!() }`)
- `mod <new>;` lines in `src/cmd/mod.rs`
- clap variants + match arms in `src/main.rs`
- help-test golden file updates if applicable

Commit message: `chore: scaffold <wave-name> surface (stub <cmd>, <cmd>)`.

See: commits `5f88113` (Pattern C) and `2806c30` (Pattern D) for shipped examples.

## Phase 1 — Plan

1. **Research subagent** (sonnet/haiku). Have it read relevant files + existing patterns and report: file paths, line ranges, key types/functions, integration points.
2. **Write the plan yourself** at `$QUIPU_VAULT/plans/YYYY-MM-DD-HHMMSS-<feature>.md`. The plan must contain **actual code** in every step — no "TBD", no "implement X appropriately".

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

### Decision tickets are dispatched with authority

```bash
./target/release/qp list --tag "kind:decision" --state ready
```

(`--tag` globs, so `"kind:decision*"` works too if you namespace them.)

Sweep these into the wave like any other ticket. A `kind:decision` body offers options with no clear winner — the tag means **"this needs a judgement call, not just implementation, and you are authorised to make it."** It is not a routing flag that sends work to the human.

Say so in the dispatch prompt, explicitly:

```
This is a kind:decision ticket. You have decision authority: weigh the
options against the real data, pick one, record it with
`qp log QP-<N> decision "<choice + why + the evidence you checked>" --auto`,
and carry through to completion. Closing won't-do with reasoning is a
legitimate resolution, and so is an outcome none of the listed options
named. Do not bounce the ticket back undecided.

If your call is one of the four loud kinds below, also run
`qp tag QP-<N> add decision:critical` — you still decide, you just mark it.
```

The agent decides and finishes. This is the normal mode, not an exception — over half of all decision events on record were made by an agent mid-wave.

**What makes an autonomous decision acceptable is the evidence, not the confidence.** A good call checks the store before choosing (e.g. confirm zero `kind:blocker` tags exist before picking a `--tag` override). A confident choice with nothing checked behind it is a guess wearing a decision's clothes — that is the failure mode to reject in a report, not the act of deciding.

Four kinds of call are loud enough that the human must see them at wrap-up:

- it would overturn a locked decision in `$QUIPU_VAULT/decisions/`
- it is irreversible or destructive with no cheap undo
- it changes a public contract other in-flight slices depend on
- there is genuinely no evidence either way and the choice is pure preference

**These are not stop-and-ask.** The agent still decides and finishes; it just marks the call so it cannot be missed:

```bash
./target/release/qp tag QP-<n> add decision:critical
```

Put that instruction in the dispatch prompt alongside the authority grant. `--tag` globs, so `qp list --tag "decision:*"` finds them all later. A tag is the right marker because tags are the pattern-agnostic extension point — the binary stays ignorant of what `decision:critical` means, exactly as it stays ignorant of `kind:decision`.

The surfacing is post-hoc, and that is the design: nothing gates, the coordinator reports at Phase 7. A pre-hoc approval clause is how tickets rot — decision-shaped tickets that need a human before dispatch sit untouched for months.

This lives here, in the skill, on purpose. `qp` does not know what a decision ticket is and must not learn — orchestration patterns stay out of the binary (CLAUDE.md). The tag is a convention this playbook reads, nothing more.

**When filing, tag honestly.** If a finding or bug report is a choice between options rather than a defect, tag it `kind:decision`, not `kind:bug`. A decision dressed as a bug looks actionable to every sweep and satisfies none of them.

## Phase 2 — Ticket (when multi-subagent)

For multi-subagent waves: open a wave ticket and child impl tickets so `qp tree`, `qp wave`, and `qp timeline` reflect the DAG.

```bash
./target/release/qp add "Wave: <feature>" --tag kind:wave
./target/release/qp add "<Slice A title>" --tag kind:impl
./target/release/qp add "<Slice B title>" --tag kind:impl

# One edge per call — `qp depends` takes a single --on, not a list.
./target/release/qp depends QP-<wave> --on QP-<a>
./target/release/qp depends QP-<wave> --on QP-<b>
```

The wave ticket depends on its slices, so it sits `pending` until they all complete and then auto-promotes to `ready`. Use the same one-edge-per-call form to express ordering *between* slices (`qp depends QP-<b> --on QP-<a>` when B must land after A) — the DAG then enforces the sequence instead of you remembering it.

**Skip ticketing** for single-subagent waves — open the impl ticket directly, no wave wrapper.

## Phase 3 — Dispatch

**First, record the wave boundary.** Capture the current max event id before anything is dispatched — Phase 7 needs it to report what this wave decided:

```bash
./target/release/qp timeline --json | jq '[.[].id] | max'   # e.g. 730
```

Keep that number. Event ids are gap-free (every event is inserted inside its mutation's `IMMEDIATE` transaction — the watch-correctness invariant in `schema.sql`), so an id captured here is an exact cut. `--since` is exclusive: `--since 730` starts at event 731.

```bash
wt switch -c wu-<slug-a> --base <branch> --no-cd --no-verify -y
wt switch -c wu-<slug-b> --base <branch> --no-cd --no-verify -y
wt list --full   # capture exact worktree paths
```

`--base <branch>` (the wave branch) is required: without it `wt switch -c` bases the slice off the default branch (`main`), so the slice would miss everything already on the wave branch (pre-scaffold, earlier-merged slices) and merge back with needless conflicts. Every slice bases off the wave branch's current tip.

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
  Report which filters you ran and that they passed — not a suite total.
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

### Do not ask subagents for suite totals

`qp-implement` forbids a bare `cargo test` because concurrent rustc graphs OOM the machine. A prompt that asks a subagent to "run the FULL suite before committing" or to report a total test count therefore instructs it to break that rule — a suite total is not obtainable from narrow filters without summing per-target runs by hand.

So: **ask subagents which filters they ran and whether those passed. Never ask for a total.** You run the full suite once at Phase 7, after all merges, with no agents live — the only place a suite total is both safe to produce and meaningful (a pre-merge total from one worktree does not describe the tree you are shipping anyway).

The OOM rule itself stays as written. "Three agents broke it and nothing burned" is not a measurement. If you want it relaxed, measure it and file a `kind:decision` ticket — do not relax a safety rule to excuse a prompt error.

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

`<target-branch>` is **this wave's branch** (from "Branch strategy") — a descriptive slug, never `main`. Pass it explicitly; `wt merge` with no target defaults to `main`, which is both wrong here and rejected by the ruleset.

```bash
wt merge -C <worktree-path> <target-branch> -y || exit 1   # target = the wave's descriptive branch, never main
```

For coordinator-direct commits (justfile edits, reactive fixes, doc-only work) that bypass the wave flow: still ticket them, and tag the ticket `branch:<name>` with the branch you landed them on. The system-of-record stays complete.

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

**Auto mode:** act only on Critical findings. Important/Minor/Observation get filed as qp tickets: `qp add "<short>" --tag kind:bug --description "<finding body>"`. Use `--tag kind:decision` instead of `kind:bug` when the finding is a choice between options rather than a defect — see Phase 1.

**Interactive mode:** triage all findings with the user, then dispatch fix subagents in parallel (one worktree per topic-affinity group via `wt switch -c fix-<slug> --base <branch>`, and `wt merge <branch> ...` back into the wave branch — same base/target discipline as slices, never `main`). After merge, mark addressed findings `**Status: FIXED in <sha>**` in the critic file.

## Phase 7 — Wrap

1. `cargo test` once, in foreground, after all merges and with no agents running:
   ```bash
   cargo test 2>&1 | grep "^test result"
   ```
2. Leanness gates: stripped-binary size, `qp --version` cold start, RSS. Confirm under budget (CLAUDE.md).
3. **Surface every decision the wave made.** Agents decide autonomously during the wave (Phase 1); this is where the human sees what they decided. Use the boundary id you captured in Phase 3:
   ```bash
   ./target/release/qp decisions --since <boundary-id>
   ./target/release/qp decisions --since <boundary-id> --auto-only   # agent-made only
   ./target/release/qp list --tag "decision:critical"                # the loud ones
   ```
   **Use `qp decisions --since`, not `timeline --kind decision`.** The alias grew `--since` and is now strictly the more ergonomic way to write the same query — same clause, same semantics. `--auto-only` composes with `--since`.

   `--since` is **exclusive**: `--since 730` starts at event 731. The Phase 3 boundary is the max id *before* anything was dispatched, so passing it exactly as captured is correct. Do not adjust it: +1 silently drops the wave's first decision, −1 pulls in a pre-wave event that is not yours to report.

   Report to the human as a short scannable list — **critical ones first and marked**, then the rest, one line each: ticket id, the choice, the one-line why. This is a report, not an approval request; the work merged in Phase 4. Anything that reads as "should I have done this?" belongs in a vault decision note, not this list.
4. Vault notes for any new decision: `$QUIPU_VAULT/decisions/<slug>.md`. If the wave reversed an earlier decision, link the two tickets as described in `qp-implement` (`qp relation add <new> supersedes <old>`) — the subagent normally does this, so here you just confirm it happened.
5. Append a session entry at `$QUIPU_VAULT/sessions/YYYY-MM-DD-HHMMSS-<slug>.md` (built / decisions / critic count / next). Use the real wall-clock time the session ends (e.g. `date +%H%M%S`) — do **not** use a daily counter like `000001`.
6. File deferred bugs as qp tickets (`qp add ... --tag kind:bug`).
7. Report to user: PR link(s), test count, decisions made, deferred items.

## Phase 8 — Ship (open the PR; the human merges)

The wave is green on its branch (Phase 7). Get it to `main` through a PR — you never push `main`, and by default you do **not** merge the PR yourself.

```bash
git -C <main-repo-path> push -u origin <branch>
gh pr create --base <pr-base> --head <branch> \
  --title "<wave title>" \
  --body "<tickets shipped, decisions made, test-count delta, leanness gates>"
```

`<pr-base>` is `main` for an independent wave, or the prior wave's branch for a stacked one — GitHub auto-retargets a stacked PR to `main` when its base merges.

Wait for `lint` to go green, then stop:

```bash
gh pr checks <pr-number> --watch
```

Report to the human: `PR #<n> — <title> — lint green, ready to merge`, and **stop there**. The behavior for this repo is *pause for the human* — `main` is public, so a person looks before it lands.

```bash
gh pr merge <pr-number> --merge
```

Which mode runs Phase 8 when (see "Branch strategy"):

- **one branch, one PR** — Phase 8 runs **once**, after the final wave's Phase 7; every wave is already on the single branch.
- **staged PR per wave** — Phase 8 runs **per wave**. In an unattended multi-wave run you keep going: branch wave N+1 off wave N (stacked), run it, open its PR, and collect the open PRs. Report them together at the end as an ordered merge list (base wave first). Do not merge them yourself unless told to.
