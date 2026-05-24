# quipu — Session Handoff Log

Append-only log of session-by-session progress, decisions, and open questions. Each entry starts with a date heading.

## 2026-05-24 — Repo bootstrap

**What was built:**
- Scaffolded standalone Rust repo at `<repo>/` (commit `f01847b`)
- clap CLI skeleton with all 20 subcommand stubs; help/version tests passing
- Workflow scaffolding migrated from orch-agents (this file, TODO.md, DECISIONS.md, wave-execute skill, CLAUDE.md, .gitignore additions) (commit `480c14e`)
- MVP plan at `docs/superpowers/plans/2026-05-23-quipu-mvp.md` (17 tasks across 7 waves)

**Scaffold deviations from the plan (Task 1 as shipped):**
- Added `"env"` to clap's features list — required by `#[arg(env = "QP_DB")]`; forced by compiler, additive.
- Added `[profile.release]` block (`lto = "thin"`, `codegen-units = 1`, `strip = true`) — supports the leanness budget (<5 MB binary). The plan's Tech Stack section now reflects this.

**Survey-driven plan refinements (folded into Task 2):**
- Schema PRAGMA expanded: `synchronous = NORMAL` + `busy_timeout = 5000` added (was just WAL + FK).
- Error type: swapped the `ConstraintError` struct for a `QuipuError` enum via `thiserror` with variants `Constraint | NotFound | InvalidInput`. `main` matches the variant for exit code (2 / 1 / 1).
- Added `src/time.rs` as the single canonical timestamp source (`now_rfc3339()` with a `Z`-suffix round-trip test). Removed `db::now()`.
- Added `map_sqlite` one-liner adapter in `db.rs`.
- Task 2 now starts with "Step 0" amendments to Task 1's already-shipped scaffold: `rust-version = "1.85"` in `Cargo.toml`, `thiserror = "1"` dep, `subcommand_required = true, arg_required_else_help = true` on the top-level `#[command]`, `//!` module doc on `main.rs`, `.cargo/config.toml` with `test-int` alias.

**Other state:**
- Vault notes auto-prefixed with `quipu-*` by Obsidian's move-disambiguation. Decision: **leave as-is** (the prefix is harmless; wikilinks updated).
- Stale duplicate plan at `orch-agents/docs/superpowers/plans/2026-05-23-quipu-mvp.md` removed; the quipu-repo copy is canonical going forward.

**Architecture decisions made:**
- See `docs/DECISIONS.md` and vault notes at `$QUIPU_VAULT/`

**Red flags / open questions:**
- None.

**Next:** Wave 1 — Foundation. Implement Task 2 (schema/db helpers/init, with the survey amendments in Step 0) and Task 3 (id encoding) from the plan. Use the `wave-execute` skill in `.claude/skills/`. Single subagent slice is fine — these are small foundation files with no parallel-conflict risk.

**Critic count:** 0 findings, 0 fixed, 0 filed.
