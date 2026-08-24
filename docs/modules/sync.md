The synopsis is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them.

```text
qp sync [--json]

Requires a ledger url:  qp remote set --ledger <url>

  1. fetch the remote ledger branch into <store-dir>/ledger
  2. merge it with whatever this machine has
  3. export this store's new events into that clone
  4. push the branch back
  5. import the batches that arrived, if any
```

## It is a composition, not a format

`sync` owns no serialisation. Step 3 is `qp export`, step 5 is `qp import
--merge`, and both are called as functions rather than reimplemented or shelled
out to. A second copy of the batch reader or of the replay order would drift
from the first, and the copy that drifts is the one whose guarantees then hold
only on paper.

What `sync` adds is the wire and the decision of when to run each half.

## Never implicit

Nothing calls this command for you. No write syncs, nothing refreshes lazily in
the background, and no other command grows a network dependency because this
one exists. A local `qp` operation is a couple of milliseconds against a local
SQLite file, and that is the reason the tool exists rather than a hosted
tracker; a sync hidden inside `qp add` would trade it away for a convenience
nobody asked for.

## One command, both directions

Publishing without fetching leaves a machine confidently stale, and fetching
without publishing leaves its work invisible to every peer. In practice the two
halves are always wanted together, so splitting them into `push`/`pull` would
mostly produce people typing both — and occasionally forgetting the second.
Each half is separately a no-op when there is nothing to do, so the combined
command costs nothing on the side that has no work.

## Divergence is the normal case

Two machines that both worked offline both hold commits the other has not seen.
That is the expected shape of a sync, not an error, and it needs no conflict
policy — because the ledger's own invariants remove the conflict:

- a batch file is named after the store that wrote it, so two stores can never
  write the same path;
- a batch is immutable, so no file is ever rewritten.

Two ledgers therefore only ever *add* files relative to each other, and a union
of adds merges with nothing to reconcile. `sync` merges with
`--allow-unrelated-histories` because on a fresh remote each machine roots its
own history, and it merges with no strategy option at all because the union is
already unambiguous. A conflict means one of the two invariants above broke, so
it aborts and changes nothing rather than picking a side.

If a peer pushes between this machine's fetch and its push, the push is
rejected; `sync` re-fetches, re-merges and retries a bounded number of times.

## Where the fetched ledger lives

`<store-dir>/ledger` — beside the database, inside the directory the project's
git already ignores. The requirement is that a sync must never leave anything
in the project tree that a person could commit; putting the clone under the
store directory satisfies it without depending on a new ignore rule being added
and kept. The test for it is direct: after a sync, `git status` in the project
repo shows nothing new.

A bare clone was considered, on the grounds that quipu reads the ledger
programmatically. It was rejected: export writes files and import reads files,
so a bare repo would need blobs written and extracted through git plumbing —
more code, more failure modes, and no additional safety over a checkout in an
already-ignored directory.

## The export is regenerated, never cached

Every sync calls `export`, which writes only events above the watermark it
derives from the filenames already on disk. With nothing new that scan finds
nothing to write and no file is touched, so a "cache" would be caching a
no-op. The batch files themselves are the cache, and they are the durable one.

## What sync must never touch

`refs/quipu/claims/*`. Claims are the authority's business and move only
through the claim path. The claim authority and the ledger are configured as
two separate urls precisely so that this command can be given one and denied
the other: `sync` reads `remote.ledger` and never `remote.authority`.

The refspecs are explicit in both directions — `refs/heads/*` inbound, the one
ledger branch outbound — and there is no `--mirror`, `--all` or tag-following
flag anywhere in the module. Mirroring claim refs would create a second referee,
and mutual exclusion with two referees is not mutual exclusion.

## Idempotence

A second sync with nothing new on either side pushes nothing, fetches nothing
new, and skips the import entirely — the merge runs only when the fetch
actually brought in a batch this store had not seen. That property is what the
whole design leans on: a sync is safe to run at any time, from any machine, in
any order.
