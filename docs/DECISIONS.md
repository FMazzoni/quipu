# quipu — Decision Log

Architectural and product decisions. Each entry links to the full record in the Obsidian vault under `projects/quipu/decisions/`.

| Date | Decision | Vault note |
|---|---|---|
| 2026-05-23 | Standalone Rust repo, per-project SQLite, binary `qp` | `decisions/standalone-rust-repo.md` |
| 2026-05-23 | CLI is pattern-agnostic; orchestration patterns live in skills | `decisions/cli-is-pattern-agnostic.md` |
| 2026-05-23 | Skills ship from the repo; `qp install-skills` places them | `decisions/skills-shipped-from-repo.md` |
| 2026-05-24 | Drop `finding`/`variant_of`; add generic `tag` + FK-integrity `relation` | `decisions/tags-and-relations.md` |
| 2026-05-24 | Liveness detection lives at the orchestrator, not the CLI | `decisions/liveness-deferred.md` |
| 2026-05-24 | Every state mutation is a single guarded conditional UPDATE | `decisions/guarded-state-transitions.md` |
| 2026-05-24 | Name: `quipu` (binary `qp`) | `decisions/naming-quipu.md` |
| 2026-05-24 | Leanness is a feature, not an afterthought | `decisions/leanness-is-a-feature.md` |
