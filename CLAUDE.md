# CLAUDE.md — quipu

## Project

quipu (`qp`) is a structured, observable task substrate for AI agent orchestration. Per-project SQLite, single static binary, pattern-agnostic. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, NOT the CLI.

## Workflow

This repo uses a three-skill split for feature work:
- **`wave-orchestrate`** (`.claude/skills/wave-orchestrate/`) — coordinator's playbook. Phases, dispatch, merge, optionally critique, wrap.
- **`qp-implement`** (`.claude/skills/qp-implement/`) — what a subagent in a worktree does. Referenced by the orchestrator in dispatch prompts.
- **`qp-critique`** (`.claude/skills/qp-critique/`) — what a critic agent does. Referenced when the orchestrator dispatches critics.

The coordinator never edits code directly; subagents do all changes inside `wt`-managed worktrees. Dogfooding convention: open qp tickets for non-trivial waves and embed slice bodies inline in subagent prompts (no separate `.tmp/QP-N.md` read targets). Bugs are qp tickets tagged `kind:bug`.

## Hard rules

- **Leanness budget:** < 30 ms cold start, < 20 MB RSS, zero external services, zero async runtime, **one optional daemon, and only the one**. PRs that exceed any of these need a justification.
- **"Zero daemons" is amended, and what survives is most of it.** The old line read "zero external services, zero async runtime, zero daemons". The premise that broke is that every quipu process is a CLI invocation that lives for milliseconds: `qp serve` is a long-lived process by construction, because fronting N stores over HTTP is not a thing a process that exits can do. **What survives unchanged, and is the load-bearing half: zero async runtime.** tokio, hyper and axum stay out of the dep tree; a synchronous, thread-per-request HTTP crate is what a server is allowed to be. The reason is the cold-start budget, which is hard: `qp` is invoked thousands of times per session and every one of those invocations would pay a runtime's initialisation to use none of it. **Zero external services also survives**, and the server does not weaken it — it fronts SQLite files that are already there, it is not a service quipu depends on, and every CLI path keeps working with no server running at all. The amendment is narrow: *one* daemon, `qp serve`, always optional, never started implicitly, never a prerequisite for anything. A second long-lived process, a background sweeper, an auto-started supervisor, or a `--daemon` flag on anything are all still refused. `docs/process-contract.md` fixes what that one process must do; a PR that adds another needs the argument this line was amended by, not this line as a precedent.
- **Binary size is an aspiration, not a cap.** The old `< 5 MB stripped` line read as a hard limit and started costing design decisions; the owner has settled that a considerably larger binary is acceptable. The rule is: pick the smallest crate that does the job *properly*, and do not contort the design — or shell out to a system binary — to save a megabyte. Cold start still matters and is still a hard budget, which is what actually rules out an async runtime and a fat SDK. Report the before/after stripped size on a PR that moves it, so the number stays visible even though it no longer gates.
- **Guarded state transitions:** every state mutation is a single conditional `UPDATE ... WHERE state IN (...)` + `changes() == 1` check. Read-then-write is banned.
- **CLI is pattern-agnostic.** Reject any PR that bakes orchestration patterns (wave, critique-loop, branch-and-evaluate) into the binary. Patterns live in `skills/`.
- **No Co-Authored-By trailer, no "Generated with Claude Code" footer** on commits.
- **No async runtime.** rusqlite-sync, anyhow, sync stdlib. No tokio, hyper, axum in the dep tree.
- **No tracing crate — and logging is `eprintln!`, now with a volume allowance for long-lived processes.** The old line read "No tracing crate. Use `eprintln!` for the rare error path." That second sentence assumed every quipu process is a CLI invocation that lives for milliseconds and errors at most once. A long-lived server has no rare error path: it runs for weeks, and its stderr is the only account of what happened to the *process* — startup, refused requests, listener faults, shutdown — none of which SQLite records. **What survives unchanged is the part that was actually load-bearing: no logging framework.** `tracing` and its ecosystem stay out of the dep tree, along with any subscriber registry, global dispatcher, or per-call-site macro expansion. The reason is the cold-start and RSS budgets, which are hard: a subscriber initialised before `main` is paid for by every one of the thousands of sub-30 ms CLI invocations that will never emit a line. **What changed is only the permitted volume:** a long-lived process may write structured operational lines on its ordinary path, not just on error. The mechanism does not change — `eprintln!` of a `serde_json`-serialised struct, one JSON object per line. `serde_json` is already a dependency and a log line needs nothing else; a level is a field in that object, never a global filter. Rotation, retention and routing belong to whatever supervises the process, not to quipu — see `docs/process-contract.md`, which also fixes what is logged and what is deliberately not.
- **Never put qp ticket ids in public artifacts.** Ticket numbers are instance-local: every quipu store numbers independently, so `QP-142` refers to something different, or to nothing, in a contributor's own store. Keep them out of PR titles and descriptions, commit messages, and any prose that renders publicly — `README.md`, `docs/`, and `//!` / `///` comments. Describe the change instead of citing a number. Example placeholders in usage docs (`qp add "..."  # → QP-1`) are fine; those illustrate output rather than cite a real ticket. A ticket links to its code through the `branch:<name>` tag and its own timestamps, recorded in quipu — never through a number embedded in git or in published prose.
- **`docs/` is compile-referenced prose plus the assets that render it.** Markdown pulled into the crate via `#[doc = include_str!()]` — currently `docs/architecture.md`, referenced from `src/main.rs` — and rustdoc build assets under `docs/assets/`. Deleting or moving a referenced file breaks the build, which is the point. Work items go to qp tickets. Do not add free-floating markdown to `docs/` — that is what QP-35 deleted, and it kept getting re-created by parallel agents.

## Commit style

Conventional commits: `feat(cmd): ...`, `fix(db): ...`, `chore(deps): ...`, `docs(readme): ...`, `test(e2e): ...`.

## When in doubt

- Run `qp decisions --json` for the decision log.
