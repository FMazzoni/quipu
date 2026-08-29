The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<dir>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp init [--prefix <P>] [--default-tag <NAME>]...   start a project here
qp init --new-project                              start one even next to a populated ledger
qp init --from <URL|DIR>                           join the project that ledger holds
qp init --from <URL|DIR> --project-key <KEY>       join one of several
qp init --from <URL|DIR> --restore                 rebuild THIS store from the ledger
qp init --project-key <KEY>                        join a project by key alone
```

## Why init declares an intent

Joining a project and starting one are different acts, and `qp init` is the only
moment they can be told apart. Afterwards there is nothing left to distinguish
them: a store that meant to join and a store that meant to start look identical,
both sync happily, and both are legal.

The failure that costs someone a week: run plain `qp init`, point it at the
team's shared ledger, sync. Nothing errors — a new project sharing a ledger is a
legitimate configuration — and the tickets filed here never reach the machines
they were meant for. It surfaces days later as "my tickets never showed up".

So init does not guess. Where a ledger it can see already holds projects, plain
`qp init` refuses and names both choices. Starting a new project on a populated
shared ledger stays possible — `--new-project` — just never accidental.

## What "a ledger it can see" means

A brand-new store has no `meta`, so there is no configured remote to read. Init
looks at what it is handed, and never at the network:

- `--from <URL|DIR>`, which is an explicit intent and needs no guard at all;
- `$QUIPU_LEDGER`, naming either.

That is the whole list, deliberately. Sniffing for something ledger-shaped
beside the store was tried and reverted: a directory named `ledger` next to a
database is not evidence of anything, and inferring from one refused fourteen
legitimate inits across the existing suites. A guard that fires on innocent
layouts is one people learn to pass a flag past, which spends its credibility
exactly where it is needed.

A url in `$QUIPU_LEDGER` is listed, which is the one place `qp init` touches the
network at all — one delimited LIST for the candidates, because the project key
is the first key segment, then one batch read per candidate to confirm it, since
a first-level segment can also be a key prefix somebody else's store publishes
under and a name in a listing is not evidence of a project — and only when that
variable is set. An unreachable ledger is
treated as no ledger rather than as an error: someone on a plane initialising a
store must not be blocked by a guard that cannot see what it is guarding. A store
that already exists is never guarded either — re-running `qp init` to add a
`--default-tag` must stay idempotent.

## What `--from` accepts

Two shapes and no guessing:

1. An `s3://<host>/<bucket>[/<prefix>]` url — the ledger proper.
2. A directory that exists — a dump `qp export` wrote, or a copied disk. This is
   the disaster-recovery path: no credentials, no network, and it has to keep
   working, because it is what someone has when the object store is the thing
   that is gone.

Anything else is refused with the url form spelled out, rather than probed as
some other transport. There is deliberately no git case any more; see
`docs/modules/sync.md` for the measurement that removed it.

Enumerating comes before downloading, which is what the object store made
possible. Choosing a project costs one listing, and only the chosen project is
ever transferred. The git ledger had to fetch everything before it could tell you
what was in it.

A join named by url ends with `remote.ledger` set to it. Someone joining from a
url has just said where the ledger is, and making them repeat it to
`qp remote set` before their first sync is exactly the ceremony this path exists
to remove.

## A join carries the history with it

Joining folds the project's batches into the new store — the same `--merge` a
sync runs, not a second merge path.

It has to, and not for convenience. A join that recorded the url and left the
history for the first sync would leave a store that never receives a single
ticket its peers wrote before it arrived — and its own board looks perfectly
healthy, which is what makes that expensive. Measured, not reasoned about: the
first version of this path did exactly that.

The join also records the object keys it read, in the same `ledger_object` table
`qp sync` keeps. Without that the first sync would download and re-merge the
entire project it has just folded in: idempotent, and a full re-fetch of history
the store is already holding.

If the merge refuses — a ledger `import` cannot legally replay — the refusal is
import's own, with its own diagnosis, and the store keeps the identity it just
established.

## An empty answer has two causes

A listing scoped outside your credentials' policy is not refused — it comes back
empty, exactly like a bucket nobody has written to. So "no batches here" would
send someone looking for a lost ledger when the real fault is a key that cannot
see it. The message names both, and names the environment variables the
credentials come from, because the usual cause is that they were exported into a
different shell.

## Joining versus restoring

Both take `--from`, and they produce different stores:

- **join** (`--from`) makes a *new machine of an existing project*: a fresh
  `project_uuid` of its own, the project's `project_key`, `display_prefix` and
  default tags inherited. It starts empty; `qp sync` brings the tickets. This is
  the second-laptop case.
- **restore** (`--from --restore`) rebuilds *this store as the one the ledger
  came from*: same `project_uuid`, same key, every event replayed. This is the
  lost-machine case, and it is `qp import --restore` fused with the init that
  has to precede it rather than a second implementation of it.

`--prefix` is refused alongside `--from` in both. The prefix comes from the
project being joined; two machines of one project that mint different prefixes
name the same ticket two different ways.

A store's project is fixed when the store is created. `qp init --project-key`
against a store that already answers to a different key fails rather than
re-homing tickets that are already published under the old one.
