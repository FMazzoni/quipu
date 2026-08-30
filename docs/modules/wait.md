This module is the wave barrier: the one place a coordinator blocks on work it
does not own. Every other command is a single transaction against one task;
`wait` is the only one whose job is to observe many tasks over time.

## Cohort barrier

`--cohort-done --tag <t>` polls a single query for two numbers over the
tag-matched set — `total`, and `non_terminal` (state NOT IN `('done',
'cancelled')`) — and returns 0 when `total > 0 && non_terminal == 0`. Both
halves are load-bearing. `cancelled` counts as drained, not as unfinished
(`wait_cohort_done_treats_cancelled_as_drained`), so an abandoned slice releases
the barrier rather than hanging it.

Multiple `--tag` values AND together; each adds an `EXISTS` subquery against
`tag`. There is no OR form.

## Why not `--state running --empty`

The obvious spelling is wrong, and `skills/wave/SKILL.md` was moved off it.
`--state running --empty` waits for zero tasks in `running`, which is true of
every *gap* in the wave — including the interval before the first agent has
claimed anything, and every moment between one agent completing and the next
claiming. The barrier releases with the work not started
(`wait_cohort_done_does_not_release_before_any_claim`) or half done
(`wait_cohort_done_does_not_release_on_staggered_claim`). Requiring `total > 0`
and counting *non-terminal* rather than *running* is what closes both holes.

`--empty` is still present and still means what it says; it is a
count-reaches-zero primitive, not a barrier. `--cohort-done` ignores `--state`
entirely.

## Exit codes 3 and 4

`wait` is the only command that owns exit codes above 2, and the only one that
calls `std::process::exit` outside `main.rs`. Code 3 is `--timeout-secs`
elapsing; code 4 is an empty cohort. Because both bypass the error path in
`main.rs`, neither produces an `{"error": ...}` envelope — `wait` is not in the
`--json` set in `wants_json`, and the exit-4 message is prose on stderr
(`wait_cohort_done_errors_on_empty_cohort`).

Exit 4 exists so that "nothing matched" and "everything finished" cannot look
alike to an orchestrator. A typo'd tag, or a barrier that starts before `qp add`
has tagged anything, would otherwise be indistinguishable from success — and an
orchestrator that reads it as success proceeds to merge work that never ran.
`--timeout-secs 0` (the default) means block forever, so there is no timeout to
catch it either.

## Polling, not locking

The loop issues one `list` every `--interval-ms` (default 500) — one SQL query
against a fresh connection locally, one round trip through a front door — and
reduces its rows to two numbers. It takes no write lock and holds no
transaction, so any number of waiters cost nothing but reads and never block the
agents they are waiting on. The trade is latency: release is detected within one
interval, not at the instant of the transition. Nothing here is
notification-driven — see `cmd/watch.rs` for the event-stream side.

*This paragraph used to say "opens one connection, prepares one statement", and
it was true of the `COUNT` this loop was before it went through `route::fetch`.
It is corrected rather than deleted because the property it was protecting is
unchanged and is the one that matters: a waiter is reads only, and it never
blocks the agent it is waiting on.*

## A front door is polled, not answered around

`wait` has no route of its own and will not get one. A served barrier is a
request that blocks for the length of a wave, and with four workers that is the
whole pool; a streaming endpoint wants an async runtime, which CLAUDE.md
forbids. So it is a *client-side poll* over the served `list`, and the same code
runs whether the store is on this disk or behind a socket — `route::fetch`
decides, and this module never learns which happened.

That structure is the point rather than a convenience. The two paths cannot
disagree about what a drained cohort is, because there is one predicate over one
payload; the alternative — a local `COUNT` and a separate remote poll — is two
implementations of one definition, and they drift.

### The failure this replaced

With a front door resolved and no route, `wait` polled the store on *this* disk.
`qp wait --tag wave:1 --empty` against a front door holding six open `wave:1`
tickets returned 0 in seven milliseconds, because the cohort is trivially empty
in a store nobody filed it into. An orchestrator branching on that exit code
proceeds to the next phase with every ticket still open, and stdout is empty in
both the true and the false case, so the only signal was prose on a stderr that
orchestration scripts routinely discard. The configuration is not exotic: this
project's own wave flow runs agents in worktrees that resolve to the project's
`.quipu` through the git-common-dir fallback, which is exactly "a client with a
local store, pointed at a server".

`route::is_client_poll` is what makes the front door reach this command at all:
it puts `wait` and `watch` on the served side of the three gates in `real_main`
that ask "does this invocation talk to a server" — the attach check, the
recorded-url tier of `resolve_server`, and `warn_unserved`. Without it a
recorded url was not even consulted, which is why that tier produced no warning
whatsoever.

### `--tag` is a glob now, because `list`'s is

The filter crosses the wire as `ListArgs::tag`, and `store::tasks` matches tags
with `GLOB` where this module's old `COUNT` used `=`. For every tag quipu itself
writes the two are the same — a glob with no metacharacter is an exact match —
and the widening is deliberate: one filter, one meaning, in argv and in SQL and
on the wire. A tag containing `*`, `?` or `[` now matches more than it did.

### Why one `list` and not a count endpoint

Measured against a loopback front door: the `list` body for a six-ticket
`wave:1` cohort is **1441 bytes**, and the server spends at most **1 ms** on the
query. At the default interval that is under 3 KB/s for a barrier, and a
count-only endpoint would save about 1.4 KB per poll in exchange for a second
protocol surface with exactly one caller. The filters already cross the wire and
are SQL clauses on the server, so a poll transfers the cohort and never the
store — which is the property that makes the trade this way round. If a
measurement ever argues for the endpoint, the number to beat is the per-poll
body of the cohort being waited on.

## Exit codes, and what a socket must not turn them into

Three exits, unchanged: 0 drained, 3 the deadline elapsed, 4 an empty cohort.
The two `std::process::exit` sites stay in this module, so those codes never
need an HTTP status and the server never learns they exist.

**A transport failure leaves the loop immediately, as exit 7.** It is not
retried and not absorbed into the timeout. A barrier that swallowed
`unavailable` and kept polling would eventually report exit 3 — "the deadline
passed" — about a front door it never reached, and confusing "nothing happened"
with "nothing was asked" is exactly what `unavailable` was introduced to
prevent. The cost is that a front door restarted mid-barrier ends the wait; the
process contract's answer applies, and it is the caller's to make.
