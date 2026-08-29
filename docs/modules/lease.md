An orphaned claim has no owner once nobody is watching. A coordinator sharing a
terminal with its agents can see a pane die and release the ticket by hand.
Machines working separate tickets have no such observer: a box that dies leaves
its ticket `running` forever, indistinguishable from work in progress. This
module is how a row says whether the machine that took it is still there.

## What a TTL bounds

**A TTL bounds how long a ticket may sit QUIET while held — not how long the
work may take.** Every write by the holder moves `last_contact` forward
(`touch`), so a ticket that takes three days but is logged against every hour
never goes stale, while one claimed and then abandoned in silence expires on
schedule.

Say it twice, because every reader gets it wrong on the first pass: **the lease
is a limit on silence, not on duration.** A slice that runs long is not at risk.
A slice whose machine died is.

## Expiry is evaluated on read

Nothing in quipu runs on a timer, and nothing may start to. There is no sweeper,
no daemon, no background thread promoting expired holds — `qp` is a process that
starts, answers one question and exits. So expiry is a *query*: `LIVE_HOLD` is a
SQL predicate evaluated whenever someone asks, and a hold that passed its expiry
an hour ago simply reads as stale the first time anyone looks. Nothing has to
have noticed in the meantime.

That is also why `LIVE_HOLD` is one exported string rather than two similar
predicates. The read path (`qp show`, `qp wave`) and `reclaim`'s guarded
`UPDATE` embed the same text, so "what a human is told is stale" and "what
`reclaim` is willing to break" cannot drift apart.

## Why the length is stored, not the expiry instant

`assignment` holds `lease_seconds` — the length the lease was taken for — and
`last_contact`. The expiry instant is derived from the pair.

Storing only the instant looks equivalent and is not: renewal has to compute a
new expiry, and with no length to renew *against* it can only reach for the
default. A deliberately short lease would silently become the default lease the
first time its holder wrote anything, which is precisely the case a short lease
was taken for.

It is also what makes the mechanism testable without a test that sleeps for
eight hours. A test writes a `last_contact` far enough in the past that the
stored length has run out, and the hold is stale on the next read.

## No stated expiry means never expires

A row with a NULL `lease_seconds` or a NULL `last_contact` is live forever.
Every open claim written before these columns existed is in that shape, and the
only safe reading of a record that never promised an expiry is that it is
unbreakable. Guessing one for it breaks a live claim, which is the failure the
whole mechanism exists to prevent.

The migration is inert for the same reason: the columns are nullable with no
default, so an existing store gains three empty columns and behaves exactly as
before until someone takes a fresh claim.

## A unit suffix is required

`30s`, `45m`, `8h`, `2d`. `parse_ttl` refuses a bare number rather than assuming
a unit. The failure it prevents is specific and has been seen: a lease meant for
hours set to that many seconds, expiring before the holder's first write.

The default is 8 hours. It was **measured against real usage**, not picked for
looking round — the quiet gaps live holders actually leave between writes run to
hours, and a default of one hour would have broken live leases repeatedly over
recorded history. Breaking a live lease reintroduces the two-holders bug this
whole mechanism exists to prevent, so the default errs long: a stale hold that
is noticed an hour late costs a wait, and a live hold broken an hour early costs
correctness.

## `holder_host`

The row records which machine holds it, not only which role. `agent_id` is a
role name that every machine playing that role reuses, so a stranded row naming
only the role cannot tell a human which box to go and look at. A server
architecture knows the machine from the connection while the connection lasts;
the row has to outlive it, and the human reading `qp show` next week is the
reason.

`QP_HOST` overrides it. That exists so a test can pin the value rather than
writing the machine it happens to run on into an assertion.

## Renewal lives in one place

`touch` is called from `db::insert_event`, which every write against a task
passes through — `log`, `tag`, `complete`, `block`, `edit`, `depends`,
`contains`, `relation` and the rest. Renewal implemented per command is renewal
that the eleventh command silently forgets, and the symptom of forgetting is a
live holder's lease being broken under it.

`touch` is also where a dispossessed holder is told. If the write cannot renew
anything and the writing agent's own last assignment on that task was closed as
`reclaimed`, `dispossessed` refuses the write and names the agent. The write
rolls back with the rest of its transaction, so the ticket is left alone: a
machine that lost its lease proceeds as a store that holds nothing rather than
wedging a ticket somebody else now owns.

## Boundaries

Only a *claimed* hold has a lease. A task sitting `assigned` and never claimed
has no holder to be stale — nobody has started, and `reclaim` releases it with
no force flag needed, exactly as before.

Nothing here recovers the work. Breaking a lease returns the ticket to the pool;
deciding whether the half-finished branch is worth keeping is a human's problem
and deliberately not modelled.
