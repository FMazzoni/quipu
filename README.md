# quipu (`qp`)

Structured, observable task substrate for agent orchestration. Per-project SQLite + small CLI. Patterns (wave, critique-loop, branch-and-evaluate) live in skills, not the binary.

## Install

    curl -sSfL https://raw.githubusercontent.com/FMazzoni/quipu/main/install.sh | sh

Downloads the prebuilt binary for your platform from the latest release, verifies
its SHA256, and installs it to `~/.local/bin` (override with `QP_INSTALL_DIR`).
Re-running upgrades in place. Linux and macOS, x86_64 and aarch64; the Linux
build is statically linked, so it runs on musl distros such as Alpine too.

To read the script before running it:

    curl -sSfL https://raw.githubusercontent.com/FMazzoni/quipu/main/install.sh -o install.sh
    less install.sh
    sh install.sh

On macOS, Gatekeeper quarantines unsigned downloads. If the binary is refused,
clear the attribute: `xattr -d com.apple.quarantine qp`.

From source, if you have a Rust toolchain:

    cargo install --git https://github.com/FMazzoni/quipu

Then `qp init` in a project.

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

## Structure: what blocks what, what is part of what

Both are `dep` edges and both make the depender wait, but they say different
things and behave differently:

    qp depends QP-3 --on QP-2               # QP-2 must finish before QP-3 starts
    qp contains QP-4 QP-1 QP-2 QP-3         # QP-1..3 are the pieces of QP-4
    qp add "slice D" --part-of QP-4         # same, at creation time

A container is not ready while anything inside it is open, so it promotes to
`ready` exactly when its last piece lands — that is a "this is finished, wrap it
up" signal, not new work. Containment nests in `qp tree`; blocking renders as an
annotation.

The difference that matters when scheduling: a blocker on a *container*
propagates down and freezes everything inside it, at any depth, while a blocker
on one piece leaves its siblings dispatchable. Where you attach the edge is the
whole of the control. If something is `pending` with an empty `blocked_by`,
`qp show` reports the culprit as `frozen_by`.

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
`reclaim`, `block`, `depends`, `contains`, `edit`, `log`, `tag`, `relation`, `init`)
accepts
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

`qp` resolves which SQLite store to operate on in four tiers:

1. `--db <path>` or the `QP_DB` env var, if set — wins outright.
2. Otherwise, the nearest `.quipu/db.sqlite` walking up from the current
   directory, stopping at the root of the checkout you are in.
3. Otherwise, `.quipu/db.sqlite` beside the repo root reported by
   `git rev-parse --git-common-dir`. This is what lets a command run from a
   worktree — a *sibling* of the main checkout, not a child — find the main
   repo's store without anyone setting `QP_DB`.
4. Otherwise, the nearest `.quipu/db.sqlite` *above* the checkout root, for a
   repo that has no store of its own but sits under a directory that does.

The boundary in tier 2 is what keeps tier 3 reachable. An unbounded walk climbs
straight out of a worktree, so any `.quipu/` in some unrelated parent directory
would outrank the repository you are actually working in — and because the wrong
store is still a valid store, reads and writes would both succeed, in the wrong
project. Stopping at the checkout root leaves a deliberately nested store (one
inside the worktree) winning, while an outsider no longer jumps the queue.

Each store stamps a `project_uuid` at `qp init`. Setting `--db`/`QP_DB` explicitly
while the working directory would have resolved to a *different* store triggers the
mismatch warning above — the guard against filing tickets into the wrong project.

## Attaching to a remote

Optional, and off by default. A store with no remote configured behaves exactly
as it always has: no network, no daemon, no external service.

    qp remote set --authority ssh://host.example/srv/authority.git
    qp remote set --ledger    ssh://host.example/srv/ledger.git
    qp remote show
    qp remote clear

Two independent urls, either one omittable:

- **authority** — where claims are refereed. A claim is a git ref push the far
  end either accepts or rejects, and that rejection is the mutual exclusion.
  Reachability matters here; ticket content never crosses this url.
- **ledger** — where the exported event log syncs. Content crosses it, so only
  machines you own need to reach it. It referees nothing.

Setting one does not require the other. Authority-only is a referee over content
that stays local; ledger-only is backup and sync with no referee. Both are
supported configurations.

`set` writes config and stops — a remote that happens to be down right now is
still a remote you are allowed to configure. `--check` adds an opt-in
`git ls-remote` probe and reports what it found; it never blocks the write.
`qp remote show` is likewise local unless you pass `--check`.

**`qp remote clear` is the escape hatch, not a convenience.** A configured but
unreachable authority makes a claim fail rather than fall back, precisely
because clearing exists as the deliberate way to get unblocked. Bare
`qp remote clear` clears both urls; `--authority` or `--ledger` clears one.

### ssh multiplexing

Where the remote is reached over ssh, quipu sets `GIT_SSH_COMMAND` on the git
child it spawns, enabling `ControlMaster` with a socket under the store's own
directory. This is measured, not cosmetic: a claim costs ~2400 ms against a
hosted remote without multiplexing and ~16 ms with it.

The socket is keyed by the acting agent's id, which matters. Multiplexing helps
*sequential* claims and breaks *bursts* — ten concurrent pushes through one
shared master all fail, because sshd's `MaxSessions` counts the master
alongside its channels. Keying by agent means one agent working through a queue
reuses its own connection, while a wave of ten agents gets ten independent
connections, which is the arrangement measured to work.

To opt out: an existing `GIT_SSH_COMMAND` in the environment is never
overwritten, and `QUIPU_SSH_MULTIPLEX=0` disables the injection entirely.

## Exporting the ledger

    qp export --out /path/to/ledger

Serialises the store's event log to plain-text JSONL under
`<out>/<project_uuid>/`, one file per export:

    <project_uuid>/<project_uuid>-0000000001-0000000128.jsonl

Each file begins with a header record carrying the store's identity
(`project_uuid`, `display_prefix`), the schema version it was written under, and
the range of event ids it holds; the remaining lines are the events themselves.

Three properties are the point:

- **Append-only and immutable.** A batch is written once and never rewritten.
  Re-running `export` emits only what is new, deriving the high-water mark from
  the filenames already present — there is no state file to fall out of sync.
- **Named by origin store.** Two stores can never mint the same filename, so
  combining two ledgers is a union of files rather than a merge of one file.
- **Readable without tooling.** The whole ledger is `cat`-able text. That is
  what a durable backup needs and what a binary snapshot cannot offer.

The event log is the complete content of the store: `tests/replay_equivalence.rs`
holds it to rebuilding `task`, `dep`, `tag`, `assignment` and `relation` exactly,
and `tests/export_roundtrip.rs` restores an exported ledger into an empty store
and asserts the two projections are identical.

Reading a ledger back is not a command yet — `qp export` is one direction, and
it makes no assumption about where the directory lives or what transports it.
Keep the destination somewhere you control: an export contains every ticket
title, description, tag and event in the store.

## Install skills into Claude Code

    qp install-skills                          # symlinks skills/* into ~/.claude/skills/qp-*
    qp install-skills --target .claude/skills  # project-local
    qp install-skills --copy                   # frozen copy

## Where next

- [`docs/architecture.md`](./docs/architecture.md) — the state machine, the guarded-transition invariant, and mutators vs projections. Its **Symptom index** (symptom → command → cause) is the place to start when something is stuck.
- `just docs` — browsable rustdoc built locally; `target/doc/qp/cmd/index.html` orients you per command (lifecycle + module→edge table).
- [`skills/`](./skills) — what `install-skills` installs: `qp-wave` (plan, dispatch, critique, loop), `qp-report-render` (Markdown/HTML from `qp report --json`).
- [`board/`](./board) — Svelte dashboard over `qp report --json`; `bun install && bun run dev`, see [`board/README.md`](./board/README.md).
- [`CLAUDE.md`](./CLAUDE.md) — the wave workflow this repo is developed with. A convention, not a feature of the binary.

## Known MVP limitations

- No liveness detection (no PID/heartbeat). Orchestrator runs `qp reclaim` on detected failures.
- Single SQLite, single machine. `qp remote` records where a claim authority and
  a ledger live, but nothing reads those urls yet: the claim-over-git path and
  `qp sync` are still to come, and `qp export` writes the interchange format
  that nothing reads back.
- Display ID prefix is fixed at `qp init` and cannot be changed afterwards (`qp init --prefix ACME`; default `QP`). Re-running `init` with a different prefix warns and keeps the original.
- Exit codes: `0` success, `1` invalid input, including argument-parse failures (and uncategorized internal failures), `2` conflict/not-owner/not-found/invariant, `3` wait timeout, `4` `wait --cohort-done` matched an empty cohort.

## License

Released under the [MIT license](./LICENSE-MIT).
