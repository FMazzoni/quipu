# quipu (`qp`)

Structured, observable task substrate for agent orchestration. Per-project SQLite + small CLI. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, not the binary.

## Install (dev)

Common workflows are wrapped in a [`justfile`](./justfile):

    just install        # build + install qp to ~/.cargo/bin
    just build          # release build only
    just test           # run the test suite
    just check-lean     # verify stripped-binary size + RSS budget

Raw `cargo install --path .` works too; the `justfile` is just a shortcut layer.

## Quickstart

    qp init
    qp add "implement parser" --tag wave:1                # → T1, state=ready
    qp add "wire CLI" --depends-on T1 --tag wave:1        # → T2, state=pending
    qp assign T1 --to alice
    qp claim   T1 --as alice
    qp complete T1 --as alice --decision "chose pest"
    qp tree
    qp timeline T1
    qp decisions
    qp wave
    qp status
    qp watch

## Tags and relations

    qp tag T1 add kind:critique           # flat label
    qp tag T1 rm kind:critique
    qp relation add T2 variant-of T1      # FK-integrity cross-task ref
    qp relation list T1

## Cleanup and recovery

    qp cancel T5 --reason "no longer needed"
    qp abandon T5 --as alice              # agent self-release
    qp reclaim T5 --reason "agent dead"   # orchestrator force-release

## Machine-readable output

Every mutating command (`add`, `assign`, `claim`, `complete`, `cancel`, `abandon`,
`reclaim`, `block`, `depends`, `edit`, `log`, `tag`, `init`) accepts `--json` and
emits a bare JSON object on success (no `{"ok":true,...}` wrapper — success is
already disjoint from error by stream and exit code) carrying the canonical
`display_id`. `qp block --json`, for instance, returns the newly created
blocker's id directly instead of requiring stdout-scraping:

    qp block T5 --as alice --new "needs design review" --json
    # {"display_id":"T5","blocker_id":"T9","blocker_title":"needs design review","state":"pending"}

On failure, errors render as prose on stderr in human mode, or as
`{"error": {"kind": ..., "code": ..., "message": ..., "task": ...}}` on stderr
in `--json` mode. `kind` is one of `conflict` (wrong state / lost race — retry
may succeed), `not_owner` (a different agent holds it — don't retry),
`not_found` (referenced entity/edge doesn't exist), `invariant` (structural
violation, e.g. a dependency cycle — replan), or `invalid_input` (bad CLI
input). `code` is a stable string for precise skill authoring (e.g.
`already_claimed`, `not_ready`, `state_changed_under_us`) and grows additively.

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

## Known MVP limitations

- No liveness detection (no PID/heartbeat). Orchestrator runs `qp reclaim` on detected failures.
- Single SQLite, single machine. Remote/sync is v2.
- Display ID prefix is fixed at `qp init` and cannot be changed afterwards (`qp init --prefix ACME`; default `QP`). Re-running `init` with a different prefix warns and keeps the original.
- Exit codes: `0` success, `1` invalid input, `2` conflict/not-owner/not-found/invariant, `3` wait timeout, `4` `wait --cohort-done` matched an empty cohort.
