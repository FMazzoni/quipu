# CLAUDE.md — quipu

## Project

quipu (`qp`) is a structured, observable task substrate for AI agent orchestration. Per-project SQLite, single static binary, pattern-agnostic. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, NOT the CLI.

## Vault auto-capture

brain_project: quipu

## Workflow

This repo uses the `wave-execute` skill (in `.claude/skills/wave-execute/`) for all feature work. The coordinator never edits code directly; subagents do all changes inside worktrees managed by `wt` (worktrunk). Plans live in `docs/superpowers/plans/`. The current MVP plan is `docs/superpowers/plans/2026-05-23-quipu-mvp.md`.

## Hard rules

- **Leanness budget:** < 5 MB stripped binary, < 30 ms cold start, < 20 MB RSS, zero external services, zero async runtime, zero daemons. See `docs/DECISIONS.md` → `leanness-is-a-feature.md`. PRs that exceed any of these need a justification.
- **Guarded state transitions:** every state mutation is a single conditional `UPDATE ... WHERE state IN (...)` + `changes() == 1` check. Read-then-write is banned. See `guarded-state-transitions.md`.
- **CLI is pattern-agnostic.** Reject any PR that bakes orchestration patterns (wave, critique-loop, branch-and-evaluate) into the binary. Patterns live in `skills/`.
- **No Co-Authored-By trailer, no "Generated with Claude Code" footer** on commits.
- **No async runtime.** rusqlite-sync, anyhow, sync stdlib. No tokio, hyper, axum in the dep tree.
- **No tracing crate.** Use `eprintln!` for the rare error path.

## Commit style

Conventional commits: `feat(cmd): ...`, `fix(db): ...`, `chore(deps): ...`, `docs(readme): ...`, `test(e2e): ...`.

## When in doubt

- Read `docs/DECISIONS.md` first.
- Vault notes at `$QUIPU_VAULT/` are the long-form reasoning behind each decision.
