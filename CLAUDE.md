# CLAUDE.md — quipu

## Project

quipu (`qp`) is a structured, observable task substrate for AI agent orchestration. Per-project SQLite, single static binary, pattern-agnostic. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, NOT the CLI.

## Vault auto-capture

brain_project: quipu

## Workflow

This repo uses a three-skill split for feature work:
- **`wave-orchestrate`** (`.claude/skills/wave-orchestrate/`) — coordinator's playbook. Phases, dispatch, merge, optionally critique, wrap.
- **`qp-implement`** (`.claude/skills/qp-implement/`) — what a subagent in a worktree does. Referenced by the orchestrator in dispatch prompts.
- **`qp-critique`** (`.claude/skills/qp-critique/`) — what a critic agent does. Referenced when the orchestrator dispatches critics.

The coordinator never edits code directly; subagents do all changes inside `wt`-managed worktrees. Plans, critiques, and session logs live in the Obsidian vault at `$QUIPU_VAULT/` (`plans/`, `critiques/`, `sessions/`, `decisions/`). The MVP plan is `plans/2026-05-23-021926-quipu-mvp.md`. Dogfooding convention: open qp tickets for non-trivial waves and embed slice bodies inline in subagent prompts (no separate `.tmp/QP-N.md` read targets). Bugs are qp tickets tagged `kind:bug`.

## Hard rules

- **Leanness budget:** < 5 MB stripped binary, < 30 ms cold start, < 20 MB RSS, zero external services, zero async runtime, zero daemons. See vault `decisions/quipu-leanness-is-a-feature.md`. PRs that exceed any of these need a justification.
- **Guarded state transitions:** every state mutation is a single conditional `UPDATE ... WHERE state IN (...)` + `changes() == 1` check. Read-then-write is banned. See `guarded-state-transitions.md`.
- **CLI is pattern-agnostic.** Reject any PR that bakes orchestration patterns (wave, critique-loop, branch-and-evaluate) into the binary. Patterns live in `skills/`.
- **No Co-Authored-By trailer, no "Generated with Claude Code" footer** on commits.
- **No async runtime.** rusqlite-sync, anyhow, sync stdlib. No tokio, hyper, axum in the dep tree.
- **No tracing crate.** Use `eprintln!` for the rare error path.

## Commit style

Conventional commits: `feat(cmd): ...`, `fix(db): ...`, `chore(deps): ...`, `docs(readme): ...`, `test(e2e): ...`.

## When in doubt

- Browse vault decisions at `$QUIPU_VAULT/decisions/` (long-form reasoning behind each decision) or run `qp decisions --json` for the live log.
- Past session handoffs in `sessions/`, plans in `plans/`, critic reports in `critiques/` — all under the same vault path.
