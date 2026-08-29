The synopsis is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them.

```text
qp sync [--json]

Requires an object-store ledger:  qp remote set --ledger s3://<host>/<bucket>[/<prefix>]

  1. LIST this project's prefix
  2. account for the objects this store wrote itself
  3. export this store's new events and PUT them as one object
  4. GET the objects this store has not seen
  5. import them, if any
```

## It is a composition, not a format

`sync` owns no serialisation. Step 3 is `qp export`, step 5 is `qp import
--merge`, and both are called as functions rather than reimplemented or shelled
out to. A second copy of the batch reader or of the replay order would drift
from the first, and the copy that drifts is the one whose guarantees then hold
only on paper.

What `sync` adds is the transport and the decision of when to run each half.

## Never implicit

Nothing calls this command for you. No write syncs, nothing refreshes lazily in
the background, and no other command grows a network dependency because this
one exists. A local `qp` operation is a couple of milliseconds against a local
SQLite file, and that is the reason the tool exists rather than a hosted
tracker; a sync hidden inside `qp add` would trade it away for a convenience
nobody asked for.

## One command, both directions

Publishing without reading leaves a machine confidently stale, and reading
without publishing leaves its work invisible to every peer. In practice the two
halves are always wanted together, so splitting them into `push`/`pull` would
mostly produce people typing both — and occasionally forgetting the second.
Each half is separately a no-op when there is nothing to do, so the combined
command costs nothing on the side that has no work.

## Why the ledger is not a git branch

Measured, not assumed. A git fetch takes a whole branch; there is no way to ask
for part of one. So every project's store carried every other project's ledger,
and total local storage grew as the square of the number of projects — fine at a
handful, tens of gigabytes per machine at a few hundred. Partial clone was tested
against a real daemon and the filter was ignored outright; sparse checkout only
hides objects the pack still holds; `git archive --remote` is commonly disabled.
The behaviour is not configurable away.

A GET names one prefix. So the ledger became objects, laid out project-first:

```text
<url prefix>/<project key>/<store uuid>/<store uuid>-<lo>-<hi>.jsonl
```

A store reads its own project and nothing else, and a project nobody has touched
in a year costs nothing. Everything the git ledger needed to make a shared branch
safe — a working tree, a merge, a commit, a push, a retry when a peer got there
first — went with it, because a PUT to a key nobody else can name is not a race.

**Claims are not in the ledger at all.** A claim is mutual exclusion between
writers, not content to be replicated; it is settled locally, by one writer
holding `BEGIN IMMEDIATE`, and never leaves the machine. `qp remote set` names
the ledger and only the ledger, which is why the ledger's transport can change
without touching the claim path.

## Divergence is the normal case

Two machines that both worked offline both hold events the other has not seen.
That is the expected shape of a sync, not an error, and it needs no conflict
policy — because the ledger's own invariants remove the conflict:

- an object key names the store that wrote it, so two stores can never write the
  same key;
- a batch is immutable, so no object is ever rewritten, and there is no DELETE.

Two ledgers therefore only ever *add* objects relative to each other, and a union
of adds has nothing to reconcile.

## There is no local mirror, and that is the point

Nothing is cached on disk. A sync downloads what is new into a temp directory
that removes itself, merges it, and is done. Keeping a copy of even this store's
own project would reintroduce in miniature the local mirror the transport change
deleted — and this store's own batches are already in its log.

The one thing that does persist is the answer to *what have I already seen*: the
`ledger_object` table, one row per object key this store published or merged. It
lives in the store because the store is already the durable, transactional thing
on this machine, and it stays prefix-scoped, so a store only ever records keys
under its own project. Losing a row is safe and costs one redundant download —
re-merging a batch is a no-op, since `foreign_event` is keyed by the origin store
and its event id. Recording one too early is not, which is why a key is written
only after the merge that read it has committed.

## The export is regenerated, never cached

Every sync calls `export`, which writes only events above a watermark. `qp
export` derives that watermark from the filenames in the directory it is writing
to; a sync cannot, because it writes into a scratch directory that is deleted
moments later. So the sync derives it from the object keys this store owns — the
same fact, from the only place that still holds it.

That is also why the LIST comes before the export rather than after. A store
whose database was rebuilt from the ledger discovers what it already published
before deciding what to publish, instead of republishing its whole history under
new names.

## One ledger, several projects

A bucket is shared on purpose: many projects sit side by side in it. What changed
is that a store no longer has to see the others in order to ignore them — `qp
sync` lists exactly one prefix, and another project's batches never reach it.

The **import** is still scoped by project key as well, and that belt-and-braces
is deliberate: a prefix says where an object was filed, and a header says what it
is. The header wins. The comparison is against `project::effective_key`, never
the raw field, so a batch written before the key existed matches only the store
it belongs to rather than whoever happens to ask. See `docs/modules/project.md`.

`qp ledger` is the one command that reads across projects, and so the one that
lists the whole bucket. That is the exception, it is a read-only query, and it
writes nothing local.

## What sync must never touch

Anything but its own project's batch keys. `sync` reads `remote.ledger`, and
`remote.ledger` is the only url a store has — a destination it was not pointed
at is not something it can reach by widening a pattern.

## Idempotence

A second sync with nothing new on either side writes no object, downloads
nothing, and skips the import entirely — the merge runs only when the listing
turned up a batch this store had not accounted for. That property is what the
whole design leans on: a sync is safe to run at any time, from any machine, in
any order.
