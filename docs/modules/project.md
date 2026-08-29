A store has two identities and they answer different questions.

- **`project_uuid`** names *this store*. It is minted per `qp init`, it appears
  in every batch filename, it is half of a claim ref name, and `task_origin`
  points at it. Two machines of one project have two of them, and they must:
  that is what keeps their batches from colliding and their tickets
  distinguishable.
- **`project_key`** names *the project*. Every machine of one project carries
  the same one. It is minted once, when someone starts a project, and inherited
  by every machine that joins afterwards.

Before this key existed, a store's only identity was `project_uuid`, so nothing
in the code separated "a peer of my project" from "an unrelated project that
happens to share a ledger". On a shared ledger that meant every project absorbed
every other project's tickets. The key is what makes scoping possible; the
scoping itself — which batches a sync folds in, how a global query groups its
results — is not implemented here and deliberately not implied by this module.

## Minted, or inherited. Never derived.

Nothing derives one identity from the other. It is tempting — a store *is* a
project when there is only one machine — and it defeats the entire point the
first time something reaches for the wrong one: two machines of one project
would be two projects again.

The one exception is deliberate and bounded: a store or a batch written *before*
this key existed gets `legacy-<project_uuid>`. Such a store is genuinely its own
project (nothing ever linked it to another one), and deriving rather than
minting makes the answer stable — the keyless batches it already published to a
ledger resolve to exactly the key its future batches will carry. A minted key
never starts with `legacy-`, so the two can never collide and reading one tells
you which era it came from.

## What a keyless batch means

Every batch written before format version 2 has no `project_key` field. Such a
batch is **not** "unknown project", and must never be treated as matching
whoever is asking — that would restore the absorb-everything behaviour under a
new name, for every ledger written before the key existed. It is the store that
wrote it, standing alone, under the legacy key above. `effective_key` is the
single place that decision is made; readers call it rather than looking at the
raw field, and `batch_key` is that reading applied to one file on disk.

## What counts as a project

A place holds a project when a batch under it carries a header this binary can
read — never when a directory or a key segment merely exists under that name.
`scan` applies that to a directory and `scan_object` to an object ledger; both
decide through the same `header`, and the transports differ only in where the
bytes come from. The rule is stated once because it was once implemented twice:
the object listing believed its own `CommonPrefixes`, so a bucket root holding a
key prefix offered a project to join that the global query did not show, and
joining it produced a store with no batches in it.

A prefix is part of the LOCATION, not of the data. Two stores pointed at one
bucket under different prefixes are as isolated as two buckets — by design — so
"what projects exist here" is only answered relative to a prefix, and the root is
not a privileged vantage point.

The reverse direction is a hard break, on purpose: format version 2 batches are
refused by a `qp` old enough to predate the key, because such a reader would
silently ignore it and merge across projects. See `docs/modules/export.md`.

## Backfill runs ungated

`project::backfill` is called on every open, outside the `schema_version` gate,
exactly like `db::add_missing_columns`. Every store that predates the key is
already stamped at a version this binary knows, so a gated backfill would never
reach a single one of them — the store would look keyless forever and every
project-scoped read would have to invent an answer for it.
