The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<dir>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp export --out <dir> [--json]

Writes every event not already on disk as one new batch file:

  <dir>/<project_uuid>/<project_uuid>-<first>-<last>.jsonl
```

## What is exported, and why it is the log

The batch carries the `event` table, not a snapshot of the projection tables.
The store's own replay-equivalence property is what makes that a complete
export rather than a partial one: the event log rebuilds `task`, `dep`, `tag`,
`assignment` and `relation` exactly, and a write that changes a projection
without emitting an event fails that property by construction. So the log is
the smaller of the two artifacts and loses nothing the tables hold.

Three properties follow from exporting the log that a table snapshot cannot
have, and they are the reason for the choice:

- **It is append-only.** Events are immutable, so a batch, once written, is
  never rewritten. A table snapshot is a picture of mutable rows and has to be
  re-emitted whole every time anything changes.
- **It merges.** Two ledgers combine by taking the union of their files. Two
  table snapshots of the same store diverge and need a row-level reconciliation
  rule; two event batches do not, because appending is commutative and replay
  is deterministic.
- **It deltas.** Unchanged batches keep their existing content, so a version
  control system storing successive exports pays only for what is new.

## Naming, and why identity is in the envelope

A batch file is named by its **origin store** — the `project_uuid` stamped at
`qp init` — and then by the range of event ids it closes over. Two stores can
therefore never produce the same filename, which is what makes merging two
ledgers a set union of files rather than a three-way merge of one file.

`project_uuid`, `project_key` and `display_prefix` are **identity**: nothing in the store ever
mutates them, so there is no change for an event log to record. They belong to
the envelope, carried in the batch header and in the filename. `schema_version`
and the default-tag set are **state** — they do change — and are carried in the
header today as a snapshot of their value at export time. Logging them as
events is the more correct treatment and is not yet done; until it is, a
restore recovers their *current* value, not their history.

## The header

Every batch begins with one header record, so a batch is interpretable on its
own with no index, catalog or sibling file:

```text
{"record":"header","format":"quipu-export","format_version":2,
 "schema_version":"5","project_uuid":"...","project_key":"...","display_prefix":"QP",
 "default_tags":[...],"task_origins":[...],"first_event_id":1,
 "last_event_id":128,"event_count":128,"exported_at":"..."}
```

`task_origins` is present only in a store that has imported: it names the tasks
whose canonical identity is not the one their rowid implies, because another
store minted them. The events in the batch carry rowids as they are *here*, so a
peer needs this to recognise a ticket it already knows under a different number
— without it, one ticket merges as two. Its absence is unambiguous rather than
unknown: a store that never imported created every task it holds.

`project_uuid` names the store that wrote the batch; `project_key` names the
project that store belongs to, and every machine of one project writes the same
one. A reader with only the first cannot tell a peer of its own project from an
unrelated project sharing the ledger — which is why adding the second moved
`format_version` to `2` rather than sliding in as a field older readers would
ignore. A `qp` that predates the key refuses a version-2 batch outright, and
upgrading every machine on a shared ledger is the price of the ledger being
shared. Batches at version 1 stay readable forever, and mean the store that
wrote them standing alone — see `docs/modules/project.md`.

`format_version` versions this file layout; `schema_version` versions the store
that produced it. Both are present because they can move independently, and a
reader that cannot tell which batch was written under which rules will either
fail to parse an old batch or, worse, misread it silently.

## Resuming

The high-water mark is derived from the filenames already in the directory, not
from a state file. There is nothing to fall out of sync with the batches
themselves: delete a batch and the next export re-emits that range; point
`--out` at an empty directory and the next export emits the whole log.

Each batch is written to a temporary name and renamed into place, so a crash
mid-write cannot leave a partial file wearing a name the high-water scan
trusts.

## What this command does not do

Reading a ledger back is `qp import` — `--restore` into an empty store, or
`--merge` into a populated one. `qp export` stays one direction and makes no
assumption about where the directory is kept or what transports it.

It also exports only what THIS store wrote. Events merged in from a peer live in
`foreign_event` and are never re-emitted here: re-exporting them under our uuid
and a fresh id would give one event two names and break the dedupe every reader
depends on. Each machine exports what it wrote, and the union of the batches is
the whole log.
