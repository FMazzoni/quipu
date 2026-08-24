The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<dir>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp init [--prefix <P>] [--default-tag <NAME>]...   start a project here
qp init --new-project                              start one even next to a populated ledger
qp init --from <LEDGER>                            join the project that ledger holds
qp init --from <LEDGER> --project-key <KEY>        join one of several
qp init --from <LEDGER> --restore                  rebuild THIS store from the ledger
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

- `--from <LEDGER>`, which is an explicit intent and needs no guard at all;
- `$QUIPU_LEDGER`, if it names an existing directory.

That is the whole list, deliberately. Sniffing for something ledger-shaped
beside the store was tried and reverted: a directory named `ledger` next to a
database is not evidence of anything, and inferring from one refused fourteen
legitimate inits across the existing suites. A guard that fires on innocent
layouts is one people learn to pass a flag past, which spends its credibility
exactly where it is needed.

So the guard is honest about its reach rather than approximate: it fires only
where a ledger directory can be enumerated and found to hold a project. A ledger
that exists only as a url nobody has cloned is invisible to init. A store that
already exists is never guarded either — re-running `qp init` to add a
`--default-tag` must stay idempotent.

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
