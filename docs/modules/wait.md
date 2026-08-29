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

The loop opens one connection, prepares one statement, and re-runs it every
`--interval-ms` (default 500). It takes no write lock and holds no transaction,
so any number of waiters cost nothing but reads and never block the agents they
are waiting on. The trade is latency: release is detected within one interval,
not at the instant of the transition. Nothing here is notification-driven — see
`cmd/watch.rs` for the event-stream side.

## A resolved front door is a refusal, not a local answer

`wait` has no route. That is deliberate — a served barrier means a request that
blocks for the length of a wave, and with four workers that is the whole pool —
but until it polls a front door itself, an invocation with one resolved must
**refuse**, exit 1, rather than answer from the store on this disk.

The failure it replaces was live and silent. `qp wait --tag wave:1 --empty`
against a front door holding six open tickets returned 0 in seven milliseconds:
it polled the *local* store, where a cohort nobody filed there is trivially
empty. An orchestrator branching on that exit code proceeds to the next phase
with every ticket still open. There was a warning on stderr, and that is not a
signal — `wait`'s entire output is an exit code, stdout is empty in both the
true and the false case, and orchestration scripts routinely discard stderr.

Three properties of the refusal, each of which was a choice:

- **Refuse, do not warn.** `warn_unserved` is right for `export` and `sync`:
  those name a path on the caller's disk, so running locally is what the
  operator meant. A barrier has no such reading, and an exit code cannot carry
  a caveat.
- **Key on a *resolved* front door, not on an explicit `--server`.** The
  recorded-url tier is the case that produced no warning at all, because
  `resolve_server` did not consult it for a verb with no route. `is_client_poll`
  in `route.rs` puts `wait` and `watch` on the served side of that gate, which
  is what makes a configured store reach this refusal instead of answering in
  silence.
- **Refuse before the attach check.** A storeless client already got the right
  answer (`no_store`, exit 6); the betrayed case is the client that *has* a
  store. Two exit codes for one misconfiguration is a distinction nothing
  downstream wants, so both now get exit 1.

The configuration is not exotic: this project's own wave flow runs agents in
worktrees that resolve to the project's `.quipu` through the git-common-dir
fallback, which is exactly "a client with a local store, pointed at a server".
