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
    just check-lean     # report stripped-binary size, cold start and RSS

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

Where several machines pull their own work rather than a coordinator handing it
out, `qp take` replaces the `list`-then-`assign` pair with one atomic step —
selection and assignment happen in a single statement, so two machines cannot
choose the same ticket:

    qp take --as alice --tag wave:1     # → assigned, or `taken: null` if the queue is drained

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
    qp reclaim QP-5 --reason "agent dead"   # release a hold that has gone stale
    qp reclaim QP-5 --reason "..." --force  # break a hold that is still live

A claim takes a lease: `qp claim QP-5 --as alice --for 45m`, defaulting to `8h`.
The suffix is required, because a bare number is ambiguous in exactly the way
that sets a lease to seconds when hours were meant.

**A lease bounds how long a ticket may sit quiet while held — not how long the
work may take.** Every write by the holder (`log`, `tag`, `block`, `complete`, …)
pushes the expiry out, so a ticket that runs for days but is written against
along the way never goes stale. One that was claimed by a machine that then died
does. Say it twice: the limit is on silence, not on duration.

Nothing sweeps on a timer. Expiry is evaluated when someone asks, so a stale hold
shows up in `qp show` and `qp wave` the next time a human or an orchestrator
looks, and `qp reclaim` releases it. A claim recorded before leases existed
states no expiry and therefore never expires — releasing one still takes
`--force`.

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
| `no_store` | `path` | no store here — not attached to a project; don't retry |
| `unavailable` | `endpoint` | the front door did not answer — no store saw the request; retry the transport |
| `internal` | — | uncategorized failure; treat as a bug |

The first seven are the `QuipuError` variants in `src/db.rs`; `internal` is not
a variant but the fallback `main.rs` emits for any error that does not downcast
to one, so a consumer matching on `kind` must handle all eight.

Exit codes: `0` success, `1` `invalid_input`/`internal`, `2` the store refused
(`conflict`, `not_owner`, `not_found`, `invariant` — retrying may succeed), `3`
`wait` timed out, `4` `wait --cohort-done` matched an empty cohort, `5`
`remote doctor` found a fault, `6` `no_store`, `7` `unavailable`. `2` and `7`
are worth retrying — `2` once the state settles, `7` once the transport comes
back; `6` never is. Every command but `qp init` refuses to create a store, so a
machine pointed at nothing fails loudly instead of quietly acquiring an empty
private project.

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

## Starting a project, or joining one

A store carries two identities, and they answer different questions.
`project_uuid` names *this store*: it is minted per `qp init`, it appears in
every batch filename, and two machines of one project must have two of them or
their batches collide. `project_key` names *the project*: every machine of it
carries the same one.

`qp init` is the only moment those two intents can be told apart, so it does not
guess:

    qp init                                     start a project here
    qp init --from s3://minio.host.example/qp   join the project that ledger holds
    qp init --new-project                       start one even next to a populated ledger

`--from` takes a ledger url or a local directory of exported batches. A url is
all a new machine has, so that is the whole setup: it lists the projects,
inherits the chosen project's key, display prefix and default tags, mints a store
uuid of its own, folds that project's tickets in, and records the ledger — so the
next command can be `qp sync` with nothing configured in between. Only the chosen
project is downloaded. `qp init --from <ledger> --restore` is the other
operation: it rebuilds *this store* as the one the ledger came from, uuid and
all, for a machine that died.

Where a ledger it can see already holds projects — `--from`, or `$QUIPU_LEDGER`
naming a url or a directory — plain `qp init` refuses and names both choices. Starting a new
project on a shared ledger stays possible; it is just never accidental. The
failure that costs a week is the silent one: init, point at the team's ledger,
sync, and discover days later that these tickets were a new project all along
and never reached anybody.

A ledger that answers with nothing has two possible causes and they need
different fixes: the bucket really is empty, or the credentials in this
environment cannot see into the prefix that holds it — a listing scoped outside
your policy is not refused, it comes back empty. The message names both.

A store written before project keys existed keeps working: it is backfilled, on
the next command of any kind, as its own project. Batches written before them
mean the same thing — the store that wrote it, standing alone — never "whoever
is asking".

## Attaching to a remote

Optional, and off by default. A store with no remote configured behaves exactly
as it always has: no network, no daemon, no external service.

    qp remote set --ledger s3://minio.host.example/quipu-ledger
    qp remote set --front-door https://qp.host.example
    qp remote show
    qp remote doctor
    qp remote clear

Two urls, unrelated to each other:

- **ledger** — where the exported event log syncs, as objects in a bucket:
  `s3://<host>/<bucket>[/<prefix>]`, https implied. Content crosses it, so only
  machines you own need to reach it. It referees nothing: claims are settled
  locally, by one writer at a time, and never leave the machine.
- **front door** — the `qp serve` process this store is reached through:
  `https://<host>[:<port>]`, or `http://` to a loopback literal only. Once
  recorded, every served verb in this store goes through it with no flag: it is
  the third of four tiers, behind an explicit `--server` and `QP_SERVER` and
  ahead of opening the store locally. `qp remote doctor` diagnoses it, and
  `qp remote clear --front-door` is the way back to a local read. It is spelled
  `--front-door` because the global `--server` flag, which routes a single
  invocation through a front door without remembering it, already owns that
  name.

A store may have both, either, or neither. Neither is local-only, which is the
default and a fully supported state.

Credentials come from the standard `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` environment variables, plus `AWS_SESSION_TOKEN` when
they are temporary. quipu never writes a credential to disk and has no config
file for one; when they are absent it names the variables it looked for.

`set` writes config and stops — a remote that happens to be down right now is
still a remote you are allowed to configure. `--check` adds an opt-in probe and
reports what it found; it never blocks the write. The ledger is probed with a
listing of its own prefix, and the probe reports the object store's own
diagnosis rather than a flat "unreachable": absent credentials, rejected
credentials, a missing bucket, a prefix outside your policy, a TLS trust failure
and a skewed clock all want different fixes. `qp remote show` is likewise local
unless you pass `--check`.

### When it does not work

`--check` answers "is it there". **`qp remote doctor` answers "why is it not"**,
which is the question that actually costs time. It runs a sequence of checks
against whatever is configured — the ledger, the front door, plus this store's
name and the project registry, which are checked whether or not a remote is
attached — stopping at the first failure within each sequence, and it NAMES the cause instead of leaving you to
reverse-engineer an error that points somewhere else.

That is the whole point. A clock a quarter of an hour out makes an object store
refuse a key that was never wrong, and the error says nothing about time. A host
that moved onto a private network stops answering on its old address with a
refusal identical to the service being down. A key scoped to a prefix that
cannot list looks exactly like a bucket that does not exist, and the two fixes
are opposite. A front door that is running and does not hold your project
refuses every request in a way that reads like the front door being down, and
one of those wants a process started while the other wants a running process
restarted with a different project list.

So each result says what was checked, what happened, and — on failure — what to
actually do. Where a check genuinely cannot separate two causes it names both
rather than guessing. It is the only command here that writes to a remote: it
PUTs one fixed probe key and reads it back, because a policy can grant a listing
and refuse a write, and sync needs both. `--json` gives the same report with a
stable name per check. Exit 0 when nothing failed, 5 when something did, so it
can gate a setup script.

**`qp remote clear` is the escape hatch, not a convenience.** It is the
deliberate, recorded way back to a local-only store. Bare `qp remote clear`
clears everything; `--ledger` and `--front-door` name the url they mean.

## Exporting the ledger

    qp export --out /path/to/ledger

Serialises the store's event log to plain-text JSONL under
`<out>/<project_uuid>/`, one file per export:

    <project_uuid>/<project_uuid>-0000000001-0000000128.jsonl

Each file begins with a header record carrying the store's identity
(`project_uuid`, `display_prefix`), the project it belongs to (`project_key`),
the schema version it was written under, and
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

Keep the destination somewhere you control: an export contains every ticket
title, description, tag and event in the store.

## Reading a ledger back

    qp import --from /path/to/ledger/<project_uuid> --restore
    qp import --from /path/to/ledger --merge

**`--restore`** rebuilds an *empty* store as the one the ledger came from,
identity included, so the result **is** the original rather than a new store
holding the same tickets. It refuses to run over live data.

**`--merge`** folds other stores' batches into a populated store. It is
idempotent — re-reading a ledger with nothing new in it changes nothing — and it
rebuilds the projection from the union of every event it knows, ordered by
`(timestamp, origin store, event id)`. That order is a function of the set
rather than of the delivery schedule, so two machines that have seen the same
batches agree whatever sequence each of them saw first.

The mode is never inferred. The two do opposite things to identity, and a store
that is empty because someone ran `qp init` in the wrong directory looks exactly
like one that is empty because a disk died.

**A ledger holding history from before the event log was authoritative cannot be
fully restored, and that is permanent.** In that era tag, dependency and
description writes emitted no event at all, so they are not in the ledger and
nothing can enumerate what is missing — an event never written leaves no trace.
A restore of such a ledger warns loudly and says that the task count can look
right while the store is materially incomplete. Where a task has no creation
event at all, import refuses by default and names it; `--force` is genuinely
best effort and rebuilds everything else.

Restoring one of those ledgers into an empty store rebuilds as far as the log
allows rather than refusing, and reports every gap it found. That asymmetry is
deliberate. A merge has live data in front of it and refuses. A restore into an
empty store has nothing to protect, so refusing would hand someone mid-recovery
an empty store instead of a partial one — and where an event records no origin
state at all, the destination it *does* record is the better evidence, so the
move is applied and reported rather than dropped. Legality stays absolute
wherever the log claims to know where a move started: a ledger whose events all
carry their origins still refuses on any anomaly, in both modes.

**A merge never deletes what the log does not mention.** It distinguishes a row
the log recorded removing from one it never knew about, so pre-fix tags, edges
and descriptions survive a sync while a logged removal still removes. That is
the default, not a flag.

Two things a merge will not do quietly. Every replayed transition is checked
twice — against the state replay actually reached, and against the state machine
itself, since an impossible pair like `pending` straight to `assigned` applies
perfectly cleanly to a task that *is* pending — and a log that implies a move
nothing can justify aborts the import instead of writing it (`--force`
overrides, and still reports). And where two machines edited the same
field starting from the same value — concurrent by definition, since both claim
the same prior state — **both** values are kept, the field is left where the two
sides last agreed, and the ticket is tagged `needs-resolution`. Picking a winner
by timestamp would silently discard a real edit, because the two clocks are not
comparable.

## Syncing with a remote

    qp remote set --ledger s3://minio.host.example/quipu-ledger
    qp sync

`qp sync` is the whole cross-machine loop in one command: list this project's
prefix on the object store, upload this store's new events as one immutable
object, download the objects it has not seen, and merge them. It owns no format
of its own — it is `qp export` and `qp import --merge` with a wire between them.

**The ledger is object storage, not a git remote, and the reason is measured.**
A git fetch takes a whole branch; there is no way to ask for part of one. So
every project's store carried every other project's ledger, and total local
storage grew as the square of the number of projects — a handful is nothing, a
few hundred is tens of gigabytes per machine. Partial clone was tested against a
real daemon and the filter was ignored; sparse checkout only hides objects the
pack still holds. A GET names one prefix, so a store reads its own project and a
project nobody has touched in a year costs nothing. Claims are not in the ledger
at all: mutual exclusion between writers is settled locally, by one writer
holding the store's write lock, and is not content to be replicated.

**It never runs by itself.** No write syncs, nothing refreshes in the
background, and no other command grows a network dependency because this one
exists. Every other `qp` operation stays a couple of milliseconds against a
local file, which is the reason the tool exists rather than a hosted tracker.

**Divergence is the normal case.** Two machines that both worked offline both
hold work the other has not seen, and that needs no conflict policy: an object
key is named after the store that wrote it and is never rewritten, so two ledgers
only ever *add* objects relative to each other and the union merges with nothing
to reconcile. There is no DELETE, and no retry-on-rejection either — a PUT to a
key nobody else can name is not a race.

**It is idempotent.** A second sync with nothing new writes nothing, downloads
nothing, and skips the merge entirely.

**It syncs one project, from a bucket that may hold several.** Sharing a bucket
between projects is fine and intended. A sync lists exactly one prefix, so
another project's batches never reach it at all; the merge is scoped by project
key as well, because a prefix says where an object was filed and a header says
what it is — and the header wins. Batches written before project keys existed are
matched as the store that wrote them, never as a wildcard, so an old ledger still
reaches its own machines and nobody else's.

**Nothing is mirrored locally.** A sync downloads what is new into a temp
directory that removes itself, so it never leaves a file you could accidentally
commit. What it does keep is one row per object key it has already published or
merged, inside the store — that is how it knows what is new without a clone to
diff. As with an export, keep the bucket somewhere you control: the ledger
contains every ticket title, description, tag and event in the store.

## Reading every project on the ledger

    qp ledger
    qp ledger --json --state ready

One ledger can hold several projects, and a store only ever syncs its own one
in — so the full set exists in exactly one place, the ledger. `qp ledger` is the
read over it, and the one command that lists the whole bucket rather than a
single prefix: it replays each project's batches into a **throwaway** database
under the system temp directory, prints, and deletes it.

**It writes nothing to your store.** Not a row, not a `meta` key, not a schema
change. Answering a question about other projects must never widen the board you
are answerable for, and a separate verb — rather than a `--global` flag on
`qp list` — is what keeps the widest possible answer from looking like the
narrowest at the call site. The output says the same thing twice more: the human
header names the ledger and states that nothing local was written, and the JSON
carries `"source": "ledger"` beside the url.

It only ever reads, and it shows the ledger **as of this read** — anything you
have not synced is not in it.

Every project numbers its tickets from 1, so several projects each hold a `QP-1`.
Output is grouped by project, and the id shown is the one the ticket's own store
shows; where two machines of a single project each minted the same number, the
id carries the head of the origin store's uuid. `--json` carries the project key
and the full origin pair on every row, so a dashboard over the whole ledger can
attribute a ticket without guessing.

## Serving a store over HTTP

    qp serve --bind 127.0.0.1:8787                   # the store this directory resolves to
    qp serve --bind 127.0.0.1:8787 --project alpha --project beta

    qp --server http://127.0.0.1:8787 add "wire it up"
    qp --server http://127.0.0.1:8787 --project alpha claim QP-7 --as w-a

`qp serve` fronts stores that already exist with an HTTP listener, in the
foreground, so that a machine which cannot reach the store file can still act on
it. The client sends a served verb through a front door when one is named, in
this order: `--server` on the command line, then `QP_SERVER` in the environment,
then the url this store recorded with `qp remote set --front-door`. With none of
the three, and for every verb the front door does not answer, the store on this
disk is opened exactly as before.

**On a machine with no store, the channel is `QP_SERVER`** — the recorded url
lives in the store, so there is nothing to read there. Add `--project` when the
front door holds more than one store; one naming a store the server did not open
at startup is `no_store`, exit 6.

**The server is not the writer.** Serialisation comes from `BEGIN IMMEDIATE`
inside SQLite and always has, so a `qp` running in a terminal against the same
file stays correct while it is being served: a store behind a server is a store
with one more writer, not a store with one writer. Racing clients get exactly
one winner and a typed `conflict` for the rest, which is the same answer they
get with no server anywhere.

**The exit codes do not change.** A conflict is exit 2 whether the store was on
this disk or behind a socket, so a script that branches on the exit code needs no
change. One code is new to the client side: **exit 7, `unavailable`** — the
front door could not be reached, so no store saw the request. Treat it as
*unknown outcome*, not as *did not happen*: read the ticket back before
re-issuing a write.

**It is unauthenticated.** `--as` is unvalidated free text, so anything that can
reach the port can act as any agent — which is no worse than anything that can
write the file, and is exactly why the bind default is loopback and `http://` is
refused for any host that is not a loopback literal. To reach it from elsewhere,
put a reverse proxy in front that terminates TLS and speaks plaintext to
loopback.

Startup is fail-fast: every project is resolved and opened before the listener
binds, so a misconfiguration is a failed process with a message rather than one
that looks healthy and answers `no_store` forever. The process writes one JSON
object per line to stderr and takes no log file — `qp serve 2>> <path>` if you
want one, or let a supervisor capture it. `GET /healthz` answers from memory and
touches no store.

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

- Liveness is a lease on the assignment row, not a heartbeat: no process is
  watched and nothing runs on a timer, so a hold is judged stale only when a
  read command asks. A machine that dies leaves its ticket held until its lease
  lapses and somebody looks.
- Single SQLite per machine. `qp export`, `qp import` and `qp sync` are the
  interchange format, its reader and the transfer between machines. Claims are
  settled per store, so two machines can both hold the same logical ticket until
  their ledgers meet — and there is no command yet to settle a marked conflict.
- A ledger containing pre-fidelity history (any store written before the event
  log recorded every write) cannot be fully restored — permanently, not pending
  a fix, and the shortfall cannot even be enumerated. `qp import` warns on every
  such ledger; restoring one into an empty store rebuilds what it can and reports
  the rest, and merging into a live store is still non-destructive.
- `qp timeline` shows locally written history only. Events merged in from a peer
  are held apart from the local log so they are never re-exported under this
  store's name, and the read commands do not union the two yet.
- Display ID prefix is fixed at `qp init` and cannot be changed afterwards (`qp init --prefix ACME`; default `QP`). Re-running `init` with a different prefix warns and keeps the original.
- Exit codes are tabulated once, under "Machine-readable output" above. This
  line used to carry a second copy and had drifted three codes behind it.

## License

Released under the [MIT license](./LICENSE-MIT).
