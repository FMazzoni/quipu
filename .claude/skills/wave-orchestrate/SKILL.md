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

Sweep these into the wave like any other ticket. A `kind:decision` body offers
options with no clear winner — the tag means **"this needs a judgement call,
not just implementation, and you are authorised to make it."** It is not a
routing flag that sends work to the human.

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

The agent decides and finishes. This is the normal mode here, not an
exception: `qp decisions --auto-only` returns 71 of 133 decision events in
this store — more than half of every call on record was made by an agent
mid-wave. Wave 10 is the worked example. QP-37, QP-41, QP-49 and QP-137 were
all "no clear winner" bodies; each was dispatched with authority and each
resolved in minutes.

**What makes an autonomous decision acceptable is the evidence, not the
confidence.** The wave-10 calls hold up because every one of them checked the
store before choosing: QP-37 picked the `--tag` override after confirming zero
`kind:blocker` tags exist in the real data; QP-41 closed won't-do after
confirming no `agent_id` anywhere contains brackets; QP-49 answered a broader
question than the one it was handed, having found the two tickets were one.
A confident choice with nothing checked behind it is a guess wearing a
decision's clothes — that is the failure mode to reject in a report, not the
act of deciding.

Four kinds of call are loud enough that the human must see them at wrap-up:

- it would overturn a locked decision in `$QUIPU_VAULT/decisions/`
- it is irreversible or destructive with no cheap undo
- it changes a public contract other in-flight slices depend on
- there is genuinely no evidence either way and the choice is pure preference

**These are not stop-and-ask.** The agent still decides and still finishes; it
just marks the call so it cannot be missed:

```bash
./target/release/qp tag QP-<n> add decision:critical
```

Put that instruction in the dispatch prompt alongside the authority grant.
`--tag` globs, so `qp list --tag "decision:*"` finds them all later. A tag is
the right marker because tags are the pattern-agnostic extension point — the
binary stays ignorant of what `decision:critical` means, exactly as it stays
ignorant of `kind:decision`.

The surfacing is post-hoc, and that is the whole design: nothing gates, the
coordinator reports at Phase 7. A pre-hoc approval clause is how tickets rot —
four decision-shaped tickets filed 2026-05-25 sat untouched for two months,
and the cause was not that they needed a human, it was that nobody dispatched
them at all.

This lives here, in the skill, on purpose. `qp` does not know what a decision
ticket is and must not learn — orchestration patterns stay out of the binary
(CLAUDE.md). The tag is a convention this playbook reads, nothing more.

**When filing, tag honestly.** If a finding or bug report is a choice between
options rather than a defect, tag it `kind:decision`, not `kind:bug`. A
decision dressed as a bug looks actionable to every sweep and satisfies none
of them — that mislabelling is the actual root cause of the May tickets.

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

The wave ticket depends on its slices, so it sits `pending` until they all
complete and then auto-promotes to `ready`. Use the same one-edge-per-call
form to express ordering *between* slices (`qp depends QP-<b> --on QP-<a>`
when B must land after A) — the DAG then enforces the sequence instead of
you remembering it.

**Skip ticketing** for single-subagent waves — open the impl ticket directly, no wave wrapper.

## Phase 3 — Dispatch

**First, record the wave boundary.** Capture the current max event id before
anything is dispatched — Phase 7 needs it to report what this wave decided:

```bash
./target/release/qp timeline --json | jq '[.[].id] | max'   # e.g. 730
```

Keep that number. Event ids are gap-free (every event is inserted inside its
mutation's `IMMEDIATE` transaction — the watch-correctness invariant in
`schema.sql`), so an id captured here is an exact cut, not an approximation.
`--since` is exclusive: `--since 730` starts at event 731.

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

**Tagging the merged SHA is a required step, not an aside.** Chain it in the same Bash call as the merge so it cannot be skipped — an untagged ticket is an incomplete merge, and Phase 7 checks for exactly this.

**Only the coordinator can tag, and only after the merge.** `wt merge` squashes *and rebases* before fast-forwarding, so every SHA on the worktree branch is rewritten on the way to the target branch. `--no-squash` does not change this — it skips the squash but still rebases, so the SHAs are still new. A SHA captured on the branch side, by anyone, names a commit that never lands on the target branch and dies at the next GC. The post-merge SHA is the only one that is real. This is why `qp-implement` forbids subagents from tagging.

Two things that make this safe, both verified:

- `qp tag` works on a `done` ticket. The subagent completing its ticket in its own final steps does not block you — no reopen, no state juggling. Tag it as-is.
- The chained `git rev-parse` runs in **your** cwd, not the worktree's (which `wt merge` has already removed). That resolves correctly only if your checkout is the main repo sitting on the target branch. If you merged from somewhere else, resolve the SHA explicitly with `git -C <main-repo> rev-parse --short=6 <target-branch>`.

The tag uses the namespace `commit:<sha>` so reverse lookup is just `qp list --tag commit:<sha>` — no new commands needed.

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

**Auto mode:** act only on Critical findings. Important/Minor/Observation get filed as qp tickets: `qp add "<short>" --tag kind:bug --tag harness:claude-code --description "<finding body>"`. Use `--tag kind:decision` instead of `kind:bug` when the finding is a choice between options rather than a defect — see Phase 1.

**Interactive mode:** triage all findings with the user, then dispatch fix subagents in parallel (one worktree per topic-affinity group via `wt switch -c fix-<slug>`). After merge, mark addressed findings `**Status: FIXED in <sha>**` in the critic file.

## Phase 7 — Wrap

1. `cargo test` once, in foreground, after all merges and with no agents running:
   ```bash
   cargo test 2>&1 | grep "^test result"
   ```
2. Leanness gates: stripped-binary size, `qp --version` cold start, RSS. Confirm under budget (CLAUDE.md).
3. **Verify every wave ticket carries its `commit:` tag.** Subagents complete their own tickets (see `qp-implement`), so they are already `done` — you are not marking them done, you are auditing that Phase 4 tagged them:
   ```bash
   for t in QP-<a> QP-<b>; do
     printf '%s %s\n' "$t" "$(./target/release/qp show $t | sed -n 1p | grep -o 'commit:[0-9a-f]*')"
   done
   ```
   A blank second column means that ticket is untagged. Two gotchas baked into that line: `sed -n 1p` rather than `head -1`, because `head` closes the pipe early and `qp` panics with `failed printing to stdout: Broken pipe`; and it reads only line 1 (the tag line) because a ticket whose *description* discusses `commit:<sha>` would otherwise match itself.
   Any ticket without a `commit:` tag means a Phase 4 merge dropped the chained tag. Backfill it now with the SHA that slice actually landed as (`git log --oneline` on the target branch), and treat the miss as friction worth a vault note — hand-backfilling is the failure mode this step exists to catch.
4. **Surface every decision the wave made.** Agents decide autonomously during
   the wave (Phase 1); this is where the human sees what they decided. Use the
   boundary id you captured in Phase 3:
   ```bash
   ./target/release/qp timeline --kind decision --since <boundary-id>
   ./target/release/qp list --tag "decision:critical"      # the loud ones
   ```
   **Use `timeline --kind decision`, not `qp decisions`.** The `decisions`
   alias is more ergonomic but takes only `--db`, `--json` and `--auto-only` —
   no `--since` — so it returns the entire history (133 events and climbing)
   and the wave's dozen drown in it. If a future `qp decisions` grows
   `--since`, prefer it here; until then the alias cannot express this query.

   Report to the human as a short scannable list — **critical ones first and
   marked**, then the rest, one line each: ticket id, the choice, the one-line
   why. This is a report, not an approval request; the work merged in Phase 4.
   Anything that reads as "should I have done this?" belongs in a vault
   decision note, not in this list.
5. Vault notes for any new decision: `$QUIPU_VAULT/decisions/<slug>.md`. If the
   wave reversed an earlier decision, link the two tickets as described in
   `qp-implement` (`qp relation add <new> supersedes <old>`) — the subagent
   normally does this, so here you are just confirming it happened.
6. Append a session entry at `$QUIPU_VAULT/sessions/YYYY-MM-DD-HHMMSS-<slug>.md` (built / decisions / critic count / next). Use the real wall-clock time the session ends (e.g. `date +%H%M%S`) — do **not** use a daily counter like `000001`.
7. File deferred bugs as qp tickets (`qp add ... --tag kind:bug`).
8. Report to user: commit range, test count, decisions made, deferred items.
