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
- [ ] **All of CLAUDE.md applies:** guarded state transitions, `with_tx` + `IMMEDIATE`, no async runtime, no `tracing` crate, no `db::now()` (use `time::now_rfc3339`), leanness budget.

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
- Committing files under `docs/` (gitignored).
- Pulling in tokio/hyper/axum/tracing — see CLAUDE.md.
