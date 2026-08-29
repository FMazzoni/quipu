# `poll`

`wait` and `watch` are the two commands whose whole job is to observe over time
rather than to run one transaction. Both are polls, and both became polls *over
a socket* when the front door arrived — which is the change this module exists
for. N processes polling a file on the same machine cost page-cache reads; N
machines polling a real link cost a round trip each, and the second number is
the one that has to be defended.

## Why the interval backs off, and why not much

The default stays 500 ms. Measured against a loopback front door a `list` round
trip for a wave-sized cohort is well under a millisecond of server time, so the
interval is not paying for the query; it is paying for the link, and 500 ms is
already two orders of magnitude above a LAN round trip. Shortening it would buy
release latency nobody asked for; lengthening it as a default would slow the
phase a caller is actually watching.

What is worth changing is the *quiet* case. A barrier waits minutes and a tail
can wait hours, and in the steady state every poll transfers an empty array. So
after `QUIET_POLLS` consecutive unchanged answers — ten seconds at the default —
the interval doubles per poll up to ten times the base, and snaps back to the
base the instant an answer differs.

The cost is stated plainly because it is real: a drain that happens after a long
quiet spell is noticed up to the ceiling late, five seconds at the default. On a
barrier whose slices run for minutes that is noise. A caller who disagrees sets
`--max-interval-ms` equal to `--interval-ms`, which turns the backoff off
entirely; that is why the flag exists rather than a `--no-backoff`, because the
ceiling is the thing an orchestrator actually reasons about.

## What is deliberately not here

**No count-only endpoint.** `wait` polls the served `list` with its `--state`
and `--tag` filters applied server-side, so a poll transfers the cohort and not
the store — the cohort a barrier watches is small and shrinking by construction.
A `/v1/count` would save a few hundred bytes per poll at the cost of a second
protocol surface for one caller, and there is no measurement that asks for it.
If one ever appears, the number to beat is the per-poll body size of the cohort
being waited on, not the size of the store.

**No retry across a transport failure.** A poll that cannot reach the front door
exits 7 immediately rather than staying in the loop. That is the whole point of
the exit codes being distinct: a barrier that swallowed `unavailable` and kept
looping would report exit 3 — "the deadline passed" — about a front door it
never reached, which is precisely the confusion between "nothing happened" and
"nothing was asked" that `unavailable` was introduced to prevent.

**No streaming.** Server-sent events or long-polling wants an async runtime, and
CLAUDE.md forbids one. It would also mean a request that occupies a worker for
the length of a wave, which at four workers is the whole pool.
