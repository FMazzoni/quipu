# quipu (`qp`)

Structured, observable task substrate for agent orchestration. Per-project SQLite + small CLI. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, not the binary.

## Install (dev)

Common workflows are wrapped in a [`justfile`](./justfile):

    just install        # build + install qp to ~/.cargo/bin
    just build          # release build only
    just test           # run the test suite, one target at a time
    just lint           # formatting + rustdoc + clippy gates, then `just test`
    just docs           # build browsable rustdoc
    just check-lean     # verify stripped-binary size + RSS budget

Raw `cargo install --path .` works too; the `justfile` is just a shortcut layer.

## Quickstart

    qp init
    qp add "implement parser" --tag wave:1                   # → QP-1, state=ready
    qp add "wire CLI" --depends-on QP-1 --tag wave:1         # → QP-2, state=pending
    qp assign QP-1 --to alice
    qp claim   QP-1 --as alice
    qp complete QP-1 --as alice --decision "chose pest"
    qp tree
    qp timeline QP-1
    qp decisions
    qp wave
    qp status
    qp watch

## Tags and relations

    qp tag QP-1 add kind:critique           # flat label
    qp tag QP-1 rm kind:critique
    qp relation add QP-2 variant-of QP-1    # FK-integrity cross-task ref
    qp relation list QP-1

## Cleanup and recovery

    qp cancel QP-5 --reason "no longer needed"
    qp abandon QP-5 --as alice              # agent self-release
    qp reclaim QP-5 --reason "agent dead"   # orchestrator force-release

## Machine-readable output

Every mutating command (`add`, `assign`, `claim`, `complete`, `cancel`, `abandon`,
`reclaim`, `block`, `depends`, `edit`, `log`, `tag`, `relation`, `init`) accepts
`--json` and emits a bare JSON object on success (no `{"ok":true,...}` wrapper —
success is already disjoint from error by stream and exit code) carrying the
canonical `display_id`. `qp block --json`, for instance, returns the newly
created blocker's id directly instead of requiring stdout-scraping:

    qp block QP-2 --as alice --new "needs design review" --json
    # {"display_id":"QP-2","blocker_id":"QP-6","blocker_title":"needs design review","blocker_tags":["kind:blocker"],"state":"pending"}

On failure, errors render as prose on stderr in human mode, or as
`{"error": {...}}` on stderr in `--json` mode. Only `kind` and `message` are
always present; the rest of the body varies by kind, so branch on `kind` before
reading any other field:

| `kind` | extra fields | meaning |
| --- | --- | --- |
| `conflict` | `code`, `task` | wrong state / lost race — retry may succeed |
| `not_owner` | `task`, `owner` | a different agent holds it — don't retry |
| `not_found` | `task` | referenced entity/edge doesn't exist |
| `invariant` | `code` | structural violation, e.g. a dependency cycle — replan |
| `invalid_input` | — | bad CLI input |
| `internal` | — | uncategorized failure; treat as a bug |

The first five are the `QuipuError` variants in `src/db.rs`; `internal` is not a
variant but the fallback `main.rs` emits for any error that does not downcast to
one, so a consumer matching on `kind` must handle all six.

`code` (on `conflict` and `invariant` only) is a stable string for precise skill
authoring (e.g. `already_claimed`, `not_ready`, `state_changed_under_us`) and
grows additively.

**Under `--json`, stderr is JSON Lines**: zero or more `{"warning": {...}}`
objects, then at most one `{"error": {...}}`. Parse it line by line — do not
`json.loads` the whole buffer. The one warning today is
`{"warning": {"kind": "project_uuid_mismatch", ...}}`, emitted when `--db`/`QP_DB`
points at a different store than the working directory would have resolved to.
In human mode both stay prose.

## Store discovery

`qp` resolves which SQLite store to operate on in three tiers:

1. `--db <path>` or the `QP_DB` env var, if set — wins outright.
2. Otherwise, the nearest `.quipu/db.sqlite` walking up from the current directory.
3. Otherwise, `.quipu/db.sqlite` beside the repo root reported by
   `git rev-parse --git-common-dir`. This is what lets a command run from a
   `wt`-managed worktree — a *sibling* of the main checkout, not a child — find
   the main repo's store without anyone setting `QP_DB`.

Each store stamps a `project_uuid` at `qp init`. Setting `--db`/`QP_DB` explicitly
while the working directory would have resolved to a *different* store triggers the
mismatch warning above — the guard against filing tickets into the wrong project.

## Install skills into Claude Code

    qp install-skills                          # symlinks skills/* into ~/.claude/skills/qp-*
    qp install-skills --target .claude/skills  # project-local
    qp install-skills --copy                   # frozen copy

## Where next

- [`docs/architecture.md`](./docs/architecture.md) — the state machine, the guarded-transition invariant, and mutators vs projections. Its **Symptom index** (symptom → command → cause) is the place to start when something is stuck.
- `just docs` — browsable rustdoc; `target/doc/qp/cmd/index.html` orients you per command (lifecycle + module→edge table).
- [`skills/`](./skills) — what `install-skills` installs: `qp-wave` (plan, dispatch, critique, loop), `qp-report-render` (Markdown/HTML from `qp report --json`), `qp-verify-docs` (check docs against the code they describe).
- [`board/`](./board) — Svelte dashboard over `qp report --json`; `bun install && bun run dev`, see [`board/README.md`](./board/README.md).
- [`CLAUDE.md`](./CLAUDE.md) — the wave workflow this repo is developed with. A convention, not a feature of the binary.

## Known MVP limitations

- No liveness detection (no PID/heartbeat). Orchestrator runs `qp reclaim` on detected failures.
- Single SQLite, single machine. Remote/sync is v2.
- Display ID prefix is fixed at `qp init` and cannot be changed afterwards (`qp init --prefix ACME`; default `QP`). Re-running `init` with a different prefix warns and keeps the original.
- Exit codes: `0` success, `1` invalid input, including argument-parse failures (and uncategorized internal failures), `2` conflict/not-owner/not-found/invariant, `3` wait timeout, `4` `wait --cohort-done` matched an empty cohort.
