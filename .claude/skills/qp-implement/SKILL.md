---
name: qp-implement
description: Subagent playbook — implement one slice of a wave inside a wt-managed worktree, log friction, complete the ticket.
allowed-tools: Read Glob Grep Bash Edit Write
---

# qp-implement

> You are a subagent in a `wt`-managed worktree implementing one slice. The prompt is your contract.

## Hard rules

- [ ] **The prompt is the contract.** Don't go searching for ambient plan files. The slice body is embedded in your prompt; if a plan file is needed, the prompt cites it with an absolute path.
- [ ] **Bare `./target/release/qp` works** from any worktree (git-common-dir fallback finds the main repo's `.quipu/`). Never set `QP_DB=...`.
- [ ] **Narrow tests only.** `cargo test --test cli -- <filter>` or a specific test file. NEVER bare `cargo test` (no filter) — other agents may be running and parallel rustc invocations OOM the machine.
- [ ] **One commit.** Conventional style (`feat(cmd): ...`, `fix(db): ...`). No Co-Authored-By trailer. No "Generated with Claude Code" footer.
- [ ] **Every file you touch exits with a `//!` header.** One short sentence, a period, then a blank `//!` line before any detail — rustdoc uses everything before that blank line as the module-list summary, so a multi-line first paragraph renders as a wall of text in the table. For a command module, the summary is which state-machine edge it implements (`claim` is the `assigned` → `running` edge). Reference files and function names, never line numbers. Fence any example containing `<placeholders>` as ```text or rustdoc deletes them. If a header grows past ~15 lines of prose, keep the one-line summary in the `.rs` and move the detail to `docs/modules/<name>.md` behind `#![doc = include_str!(...)]` — with a blank `//!` line before the pointer, or the summary runs on into the detail.
- [ ] **All of CLAUDE.md applies:** guarded state transitions, `with_tx` + `IMMEDIATE`, no async runtime, no `tracing` crate, no `db::now()` (use `time::now_rfc3339`), leanness budget.

## What to write in that header

The rule above says where prose goes. This says what is worth putting there.

**Write the durable half** — what the code cannot say for itself:

- **WHY** — the reason it has this shape, and what was rejected. A reader can
  see what `with_tx` does; they cannot see that read-then-write was banned
  deliberately, or that `IMMEDIATE` is there so two agents racing on the same
  ticket fail fast instead of deadlocking mid-transaction.
- **INVARIANTS** — what must stay true, and what breaks when it does not. State
  the consequence, not just the rule: "every transition is one conditional
  `UPDATE ... WHERE state IN (...)` plus a `changes() == 1` check — a
  read-then-write here lets a concurrent claim silently win and the loser
  reports success."
- **GOTCHAS** — the thing that looks wrong but is correct, or looks safe but is
  not. If you had to work something out while writing the code, that is the
  paragraph worth keeping. The blank `//!` line before a `#![doc = include_str!]`
  pointer is exactly this: invisible, load-bearing, and a reader will delete it.
- **BOUNDARIES** — what deliberately does *not* belong here, and where it lives
  instead. "Wave sequencing is not modelled in the binary; it lives in
  `skills/wave-orchestrate/`."

**Do not restate the code.** `/// Returns the task id` above
`fn task_id() -> i64` is negative value: staleness surface carrying no
information. If deleting a sentence would lose nothing a reader could not get
from the signature or the body, delete it.

**The test:** if this code were rewritten tomorrow in a different shape, would
the sentence still be true *and* still be useful? If yes, it is durable. If it
only describes the current arrangement of lines, it will rot — leave it out.

## First three commands

```bash
cd <worktree-path-from-prompt>
cargo build --release            # so ./target/release/qp works
./target/release/qp claim QP-<N> --as <agent-id-from-prompt>
```

The `claim` records the assignment; subsequent `qp log` calls auto-attribute to you (commit `229c23c`), so you don't need `--as` on log calls during your own work.

## Work loop

For each step in the embedded slice body:

1. **Implement** — concrete code, exactly what the slice specifies. Nothing more (YAGNI).
2. **Narrow test** — `cargo test --test cli -- <filter>` for CLI integration, or `cargo test --lib <module>::tests::` for units. If a sibling slice's API isn't merged yet, that's expected — note it in your report, don't try to stub it.
3. **Commit** — one cohesive commit per slice (conventional). Squash WIP locally before reporting if you made multiple.

If you hit ambiguity:
- A judgement call within the spec's scope: make the call, log the choice as a friction note.
- A genuine contradiction in the prompt, or missing context that blocks all forward progress: report **NEEDS_CONTEXT** or **BLOCKED**.

## Required final steps (in order)

```bash
./target/release/qp log QP-<N> decision "<one-sentence friction note>" --auto
./target/release/qp complete QP-<N> --as <agent-id>
```

The `--auto` flag marks the log entry for `qp decisions --auto-only`. This feeds the live retro via `qp timeline`. Friction notes should capture what was unobvious — a surprise, a near-mistake, a place the spec was ambiguous. "Everything went smoothly" is a valid (and welcome) note when true.

`qp log` no longer needs `--as` while you hold an active assignment — it auto-attributes to the running assignee.

**Those two commands are the whole list — do not add a `qp tag ... commit:<sha>` step.** Tagging the ticket with its commit SHA is the coordinator's job, after the merge. `wt merge` squashes *and rebases* before fast-forwarding, so every SHA on your branch is rewritten on the way to the target branch: a `commit:` tag you add here names a commit that never lands and dies at the next GC. `--no-squash` does not save you — it skips the squash but still rebases, so the SHAs are still new. Leave the ticket untagged and complete it normally. `qp tag` works on a `done` ticket, so your completing it costs the coordinator nothing.

## Reporting back to coordinator

```
**Status:** DONE | DONE_WITH_CONCERNS | BLOCKED | NEEDS_CONTEXT

**Per-task summary**
- Task 1: <one line — what landed>
- Task 2: <one line>

**Tests**
<paste output of the narrow cargo test command(s)>

**Friction note** (also logged via qp log --auto)
<one sentence>

**Files changed**
- src/cmd/foo.rs (new)
- src/main.rs (+3 lines)

**Sibling APIs referenced** (coordinator merge order matters)
- depends on Slice A's `db::insert_dep` (signature: `fn(&Tx, i64, i64) -> Result<()>`)

**Concerns** (only if DONE_WITH_CONCERNS)
- <what felt fragile / what the next maintainer should know>
```

### Status meanings

- **DONE** — everything in the slice landed, narrow tests pass, friction logged, ticket completed.
- **DONE_WITH_CONCERNS** — landed and tests pass, but you noticed something the coordinator/critic should look at (a fragility, a spec ambiguity you resolved one way, a missing test case).
- **BLOCKED** — cannot proceed without coordinator intervention. State *exactly* what would unblock you.
- **NEEDS_CONTEXT** — a specific piece of information is missing; the coordinator can SendMessage it back to you.

## Anti-patterns (don't do these)

- Reading `.tmp/QP-<N>.md` or any per-ticket scratch file. That convention is dead. If the prompt doesn't say it, it isn't part of your job.
- Setting `QP_DB=...` to "make qp work from this worktree". Bare `qp` works.
- Running `cargo test` with no filter while other agents are active.
- Adding `cargo test --workspace` (this isn't a workspace).
- Squashing your friction note ("nothing notable") when something *was* notable. The retro reads these.
- Putting durable knowledge in repo files. Plans, critiques, sessions all live in the vault now (`$QUIPU_VAULT/`). Bugs are qp tickets tagged `kind:bug`.
- Pulling in tokio/hyper/axum/tracing — see CLAUDE.md.
- Tagging your ticket `commit:<sha>` from the worktree. The rebase rewrites that SHA; the coordinator tags the real one post-merge.
