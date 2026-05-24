---
name: wave-execute
description: Execute a wave — plan, dispatch parallel subagents in worktrees, merge, critique, fix, commit. The project's standard execution cycle.
allowed-tools: Read Glob Grep Bash Agent TaskCreate TaskUpdate TaskGet TaskList Edit Write
---

# Wave Execute

> **Note for quipu:** quipu is a single-crate standalone repo, not a Cargo workspace. The skill's warnings about `cargo test --workspace` causing OOM still apply if you spawn many parallel subagents in worktrees, but `cargo test` (no `-p`) inside this repo is safe since there's only one crate. Use `cargo test --test cli` for narrow integration tests.

Run a single wave cycle: plan → parallel subagent implementation → merge → critique → fix → commit.

The coordinator (you) NEVER edits code directly. All code changes happen through subagents. You read, review, merge, and coordinate.

## Phase 1: Plan

**Goal:** Produce a concrete implementation plan with independent slices that can run in parallel worktrees.

1. **Research (subagent).** Dispatch an Explore subagent to read:
   - The relevant architecture docs for this task
   - The current implementation files that will be modified
   - Any related test files
   Report: file paths, line ranges, key types/functions, integration points.

2. **Write the plan yourself** (you have the context from the research). The plan goes in `docs/superpowers/plans/YYYY-MM-DD-<feature-name>.md`. Format:

```markdown
# [Feature Name] Implementation Plan

**Goal:** [One sentence]
**Architecture:** [2-3 sentences]

---

## File Structure

| File | Responsibility | Changes |
|---|---|---|

---

## Slice A: [Name] — independent, can run in parallel

### Task N: [Component]

**Files:**
- Modify: `exact/path:line-range`

**Steps:**
- [ ] Step with actual code (no placeholders, no "implement X appropriately")
- [ ] Test code (full test bodies, not "write tests for the above")
- [ ] `cargo test -p crate -- test_name` to verify
- [ ] Commit message

---

## Slice B: [Name] — independent, can run in parallel

...

## Integration (sequential, after merge)

### Task N: [Integration step]
- [ ] Steps that depend on both slices being present
- [ ] Full workspace test
- [ ] Commit
```

**Plan rules:**
- Every step has actual code — no "TBD", "add appropriate handling", "similar to Task N"
- Group tasks into independent slices that can run in parallel worktrees
- Identify which tasks MUST be sequential (touch same files, depend on each other)
- No prompting the user for execution style — always subagent-driven

## Phase 2: Implement

**Goal:** Dispatch subagents in isolated worktrees, one per independent slice.

### Worktree lifecycle — managed by `wt` (worktrunk)

**Do NOT use `isolation: "worktree"` on Agent calls.** That creates auto-named
worktrees under `.claude/worktrees/` that leak and require force-cleanup.

Instead, the coordinator manages worktrees explicitly with `wt`:

**1. Create all worktrees before dispatching:**

```bash
# One per slice, named after the work unit
wt switch -c wu-3b --no-cd --no-verify -y
wt switch -c wu-3c --no-cd --no-verify -y
wt switch -c wu-4a --no-cd --no-verify -y
```

`--no-cd` keeps the coordinator in the main worktree. `--no-verify` skips
hooks (the subagents will run tests themselves). `-y` skips prompts.

**2. Find worktree paths:**

```bash
wt list --full   # or: git worktree list
```

The paths follow the worktrunk config template (typically
`../<repo>-<branch>/` or a configured pattern). Read the output to get
the exact paths.

**3. Dispatch subagents WITHOUT `isolation: "worktree"`:**

Pass the worktree path in the prompt so the agent works there. The agent
must `cd` to the worktree path or use absolute paths. Example:

```
prompt: |
  You are implementing WU-3B. Work in this directory:
  <scratch>

  All file paths are relative to that directory. Run tests from there.
  Commit your work when done.
  ...
```

**4. After all agents complete, merge and clean up in Phase 3.**

### Subagent dispatch rules

- **Never use `isolation: "worktree"`.** Always use `wt`-managed worktrees.
- **Parallel:** Independent slices dispatch simultaneously (one Agent call per slice)
- **Sequential:** Dependent tasks within a slice are handled by the same subagent in order
- **Model selection:** Use `sonnet` for mechanical tasks (1-2 files, clear spec). Use default (opus) for integration, multi-file coordination, or judgment calls.
- **Fix agents also get worktrees** via `wt switch -c fix-waveN --no-cd --no-verify -y`.
- **Subagents NEVER run `cargo test --workspace` or `cargo clippy --workspace`.** That triggers a full workspace build in every worktree; with N agents running in parallel it spawns N×(cores) rustc processes and OOMs the machine. Subagents may only run narrow checks scoped to the crate(s) they touched:
  - `cargo test -p <crate> -- <optional filter>` — only the crate they modified
  - `bun run check` — for orch-web slices (cheap, no rustc)
  - Specific test files: `cargo test -p orch-api --test openapi`
  The coordinator runs the workspace-wide test once after all merges land (Phase 6). Subagent prompts MUST spell out which narrow command to run; never let an agent infer that `cargo test` (no `-p`) is fine.

### Implementer prompt template

Provide each subagent with:

```
You are implementing [Slice Name] for the orch-agents TUI.

**Working directory:** [ABSOLUTE PATH to wt-managed worktree]
All commands and file paths must use this directory. cd there first.

## Tasks

[FULL TEXT of all tasks in this slice — paste it, don't make them read the plan file]

## Context

[Where this fits, what files exist, key types/functions they'll interact with]
[Architectural context they need to understand the "why"]

## Rules

- Implement exactly what the tasks specify, nothing more
- Write tests with full assertions (not just "it doesn't panic")
- Follow existing patterns in the codebase
- No Co-Authored-By trailer on commits
- If something is unclear or you're stuck, report BLOCKED or NEEDS_CONTEXT
- Self-review before reporting: completeness, quality, YAGNI

## Verification (narrow only — coordinator owns workspace-wide)

You MUST verify with the specific commands listed in your Tasks (e.g.
`cargo test -p orch-api --test openapi`, `bun run check`). You MUST NOT
run `cargo test --workspace` or `cargo clippy --workspace` — those
trigger a full workspace build in every worktree and OOM the machine
when N agents run in parallel. The coordinator runs the full-workspace
pass once after all merges.

## Report

- **Status:** DONE | DONE_WITH_CONCERNS | BLOCKED | NEEDS_CONTEXT
- What you implemented and tested
- Test results (paste the output of the narrow `cargo test -p <crate>` / `bun run check` commands listed in Tasks)
- Files changed
- Any concerns
```

### Handling subagent results

- **DONE:** Proceed to merge.
- **DONE_WITH_CONCERNS:** Read concerns. If correctness-related, address before merge. If observations, note and proceed.
- **NEEDS_CONTEXT:** Provide missing context via SendMessage to the same agent.
- **BLOCKED:** Assess: wrong model? Task too large? Plan wrong? Re-dispatch with more context, stronger model, or break the task down. Escalate to user if genuinely stuck.

## Phase 3: Merge

**Goal:** Bring worktree branches onto main, resolve conflicts, verify combined tests pass.

Use `wt merge` for each slice. It squash-commits, rebases onto main,
fast-forward merges, and cleans up the worktree + branch — all in one command.

### Merge order

Merge foundational slices first (data model, core types), then feature
slices that build on them. This matters because `wt merge` rebases onto
the current main HEAD — earlier merges become the base for later ones.

### Steps

1. **Check each worktree branch:**
   ```bash
   wt list --full   # overview of all worktrees
   git -C <worktree-path> log --oneline -5  # verify commits per slice
   ```

2. **Merge one slice at a time** using `wt merge`:
   ```bash
   wt merge -C <worktree-path> -y
   ```
   `-C <path>` runs from the worktree without cd-ing there. `-y` skips
   confirmation prompts. This squashes all commits into one, rebases onto
   main, fast-forward merges, and removes the worktree + branch.

   If rebase conflicts occur, `wt merge` will abort. Resolve manually:
   ```bash
   cd <worktree-path>
   # resolve conflicts
   git add .
   git rebase --continue
   wt merge -y   # retry from inside the worktree
   ```

3. **Verify cleanup:** `wt list` should show no leftover worktrees from
   this wave. If any remain (e.g. from a failed merge), clean up with
   `wt remove -C <path>`.

(The workspace-wide test pass is deferred to Phase 6 / wrap-up — see
"Workspace verification (coordinator-owned)" below. Don't run
`cargo test --workspace` here; it interferes with any still-running
agents and doubles the build cost.)

### If `wt merge` doesn't fit

For critic agents that only write to `docs/critic/` (gitignored), there's
nothing to merge. Just let the worktree be cleaned up with `wt remove`
after reading the output.

## Phase 4: Critique

**Goal:** Catch bugs, spec divergence, and code smells through parallel critic agents.

Dispatch **up to 4 critic subagents in parallel**, each with a different lens. Choose lenses appropriate to the wave:

| Lens | Focus | When to use |
|---|---|---|
| **Architecture** | Module boundaries, coupling, state model, impossible states | New modules, state machines, cross-module integration |
| **Correctness** | Bugs, panics, edge cases, off-by-ones, race conditions | Always |
| **UX** | Key bindings, layout, discoverability, narrow-terminal behavior | TUI changes |
| **Spec compliance** | Plan vs implementation divergence, missing/extra features | Always |
| **Performance** | Allocation, iteration cost, unnecessary clones, hot-path overhead | Data structures, event processing |
| **API surface** | Public API consistency, naming, forward-compatibility | New public types/methods |

**Critic prompt template:**

```
Review the [LENS] aspects of commits [BASE_SHA]..[HEAD_SHA] in the orch-agents project.

## What was built

[Brief description of the feature]

## Files to review

[List of changed files with line ranges]

## Your job

Read the changed code and evaluate from a [LENS] perspective.

For each finding:
- **Severity:** Critical | Important | Minor | Observation
- **Issue:** What's wrong (with file:line reference)
- **Recommendation:** How to fix

Write your findings to `docs/critic/waveN-feature-LENS.md` with this format:

---
# Wave N: [Feature] — [Lens] Critique

Commits reviewed: `BASE_SHA..HEAD_SHA`
Files reviewed: [list]

## Findings

### 1. [Title]
**Severity:** Critical | Important | Minor | Observation
[Description with file:line references]
**Recommendation:** [How to fix]

---

## Summary Table

| # | Finding | Severity |
|---|---------|----------|
---
```

Critic agents write directly to `docs/critic/`. These files are gitignored and never committed.
Critic agents do NOT need worktrees — dispatch them without isolation since
their output is gitignored and doesn't need merging.

## Phase 5: Fix

**Goal:** Address findings from the critique. Behavior depends on mode.

### Interactive mode (default)

1. **Triage findings** yourself (coordinator). Read all critic reports and present to user:
   - **Fix now:** Critical bugs, data loss, spec violations
   - **File as bug:** Important/Minor issues that aren't blocking — stays in `docs/critic/` as the record, also written to `docs/bugs/`
   - **Dismiss:** Not actionable, already correct, theoretical
2. **Get user confirmation** on the triage.
3. **Dispatch fix subagents in PARALLEL** — same pattern as Phase 2 implementation. Group "fix now" items into independent slices by file/topic affinity (e.g. "TUI render fixes" / "auth hardening" / "API rename refactor" / "session-lifecycle guards"), create one worktree per slice with `wt switch -c fix-<slice>-name`, dispatch all agents in a single message with one Agent call each. **Do NOT batch all fixes into one serial agent** — it underuses parallelism and stalls the wave (one agent doing 9 fixes serially blocks the wave for 30+ minutes when 3 agents could finish in 10). Where two slices share files, give one slice the file-level changes the other depends on, or sequence those two specific slices — but parallelize everything else.
4. **Merge fixes** onto main in dependency-safe order (mirrors Phase 3), run full test suite.
5. **Update critic files:** Mark addressed findings in the critic file (add `**Status: FIXED in [commit]**` to each addressed finding).

### Autonomous mode (unattended)

1. **Read critic reports.** Only act on **Critical** severity findings.
2. **Dispatch fix subagents in PARALLEL** in worktrees for Critical items only — same parallel-slice pattern as interactive mode.
3. **Merge fixes** onto main, run full test suite.
4. **Update critic files:** Mark Critical findings as fixed. Leave everything else for the next interactive session.
5. **Move on** to Phase 6. Do not block on Important/Minor findings.

### Bug tracking

Non-blocking issues worth tracking beyond the critic file go in `docs/bugs/` as individual markdown files:

```markdown
# [Short description]

**Source:** Wave N critique ([lens])
**Severity:** Important | Minor
**File:** `crate/path/file.rs:line`

**Issue:** [What's wrong]

**Recommendation:** [How to fix]
```

Filename format: `YYYY-MM-DD-short-description.md`. These are NOT committed to git (docs/ is gitignored). They persist locally as a backlog.

### What goes in TODO.md vs docs/bugs/

- **TODO.md:** Only feature work, phase-level items, verification gates. The project roadmap.
- **docs/bugs/:** Critique findings, code smells, minor correctness issues, UX polish. Things that should be fixed but aren't blocking forward progress.

Do NOT add critique findings to TODO.md unless they represent a missing feature or block the current phase.

## Phase 6: Wrap Up

1. **Workspace verification (coordinator-owned, serial).** Run ONCE, in the foreground, after every wave merge is complete and no subagent is still working:
   ```bash
   cargo test --workspace 2>&1 | grep "^test result" | awk '{p+=$4; f+=$6; i+=$8} END {printf "passed=%d failed=%d ignored=%d\n", p, f, i}'
   ```
   Optionally also `cargo clippy --workspace --no-deps`. Wait for any background agents to finish first — never overlap a workspace cargo invocation with running implementation/fix agents (they each spawn their own rustc and the machine OOMs).

   If the workspace run takes long enough to be noticeable, prefer `run_in_background: true` on the Bash call and use the completion notification rather than blocking the coordinator.

2. **Update `docs/TODO.md`:**
   - Check off the completed item (change `- [ ]` to `- [x]`)
   - Only add new items if they're feature-level work for a future wave
3. **Update architecture docs** if significant design decisions were made during this wave. Use `/arch` for non-trivial updates. Skip for mechanical features that follow existing patterns.
4. **Update `docs/HANDOFF.md`:** Append to the session log:
   - What was built (commit range, brief description)
   - Architecture decisions made (if any)
   - Red flags or open questions for the next session
   - Bug/critic count: "N critic findings, M fixed, K filed as bugs"
5. **Report to user:** What was done, test count, any deferred items.

---

## Coordinator Rules

- **Never edit code yourself.** All code changes go through subagents in worktrees. The ONE exception: resolving merge conflicts during `wt merge`.
- **Never invoke superpowers skills.** This skill replaces them for wave execution.
- **Never use `isolation: "worktree"` on Agent calls.** Use `wt switch -c` to create worktrees, pass the path to agents, and `wt merge` to integrate.
- **Critic agents don't need worktrees.** They only write gitignored docs — dispatch without isolation.
- **Never commit docs/ files.** `docs/` is gitignored. Critic output, bug files, plans, and architecture docs are local-only working documents.
- **Merge conflicts are expected** between parallel worktrees. Resolve them during `wt merge` rebase step.
- **One critique pass with parallel critics.** Up to 4 critics with different lenses, dispatched simultaneously.
- **No prompting for execution style.** Always subagent-driven, always worktrees.
- **No Co-Authored-By trailer, no "Generated with Claude Code" footer** on commits.
- **Workspace builds are serialised through the coordinator.** Never run `cargo test --workspace` / `cargo clippy --workspace` / `cargo build --workspace` while subagents are active — every worktree triggers an independent rustc graph and the machine OOMs. Subagent prompts must spell out narrow `-p <crate>` commands. The coordinator runs the one workspace pass at the end of Phase 6, in serial.

## Model Selection

| Task | Model |
|---|---|
| Research / Explore subagents | sonnet or haiku |
| Mechanical implementation (1-2 files, clear spec) | sonnet |
| Integration / multi-file / judgment calls | opus (default) |
| Critic reviewers | sonnet |
| Fix agents | sonnet for targeted fixes, opus for complex ones |
