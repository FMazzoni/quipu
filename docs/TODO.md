# quipu — Work Queue

Phase-level items only. Critique findings live in `docs/critic/` (gitignored); bugs live in `docs/bugs/` (gitignored). This file tracks feature work and verification gates.

## In progress

- [ ] Wave 1 — Foundation: Task 2 (schema + db helpers + init) + Task 3 (id encoding). See `docs/superpowers/plans/2026-05-23-quipu-mvp.md`.

## Backlog (from MVP plan)

- [ ] Wave 2 — Mutations: Tasks 4 (add) → 7 (cancel/abandon/reclaim)
- [ ] Wave 3 — Events & metadata: Task 8 (log/tag/relation)
- [ ] Wave 4 — Reads: Tasks 9 (timeline/decisions) + 10 (tree/status/list)
- [ ] Wave 5 — Live views: Tasks 11 (wave) + 12 (wait) + 13 (watch)
- [ ] Wave 6 — Skill + e2e: Tasks 14 (default wave skill) + 15 (install-skills) + 16 (e2e test)
- [ ] Wave 7 — Docs: Task 17 (README)

## Verification gates

- [ ] Binary size < 5 MB stripped (per `decisions/leanness-is-a-feature.md`)
- [ ] Cold start < 30 ms (`qp --version`)
- [ ] No async runtime in dep tree (`cargo tree | grep -E 'tokio|async-std|hyper'` empty)
- [ ] End-to-end wave test passes (Task 16)

## Done

- [x] Task 1 — Scaffold quipu repo with clap skeleton (commit f01847b)
