The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<dir>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp import --from <dir> --restore [--force] [--json]
qp import --from <dir> --merge   [--force] [--json]
```

`<dir>` is either one store's batch directory (`<ledger>/<project_uuid>`) or a
ledger root holding several. Batches are read one directory level deep; a file
whose first line is not a `quipu-export` header is somebody else's and is
skipped.

## Two modes, and why neither is inferred

**`--restore`** rebuilds an *empty* store as the one the ledger came from. It
adopts the envelope — `project_uuid`, `project_key`, `display_prefix`,
`schema_version`, the default-tag set and the origin map — so the result **is** the original store
rather than a new store holding the same tickets. The events go back into
`event` under their original ids, so the next `qp export` resumes exactly where
the lost one stopped. It refuses to run when any projection table holds a row.

**`--merge`** folds other stores' batches into a populated store. This is the
receiving side of sync.

A batch written before project identity existed carries no `project_key`. It is
restored under the key its own store is understood to have rather than under a
freshly minted one, because minting would quietly rename the project that every
batch that store already published belongs to. `project::effective_key` is the
single place that decision is made.

The mode is never guessed from whether the store looks empty. The two do
opposite things to identity — one overwrites it, one preserves it — and a store
that is empty because someone just ran `qp init` in the wrong directory looks
exactly like one that is empty because a disk died.

## What a merge does

1. **Records the incoming events.** Every event carries the uuid of the store
   that minted it; `(origin_uuid, origin_id)` is its name everywhere. That pair
   is the primary key of `foreign_event`, so importing the same batch twice
   inserts nothing the second time. Overlapping batches are legal — deleting a
   batch makes the next export re-emit its range — and dedupe by name is what
   makes them harmless.
2. **Rebuilds the projection from the union.** Every event this store knows,
   local and foreign, replayed in one order. Rebuilding rather than appending is
   what makes the outcome independent of the order batches arrived in: the
   merged log is a set, and a set plus a deterministic order is a sequence every
   machine derives identically.
3. **Refuses to write a projection it could not justify** — see *Anomalies*.

Foreign events are deliberately kept out of `event`. That table is what
`qp export` writes out; re-exporting a peer's event under our uuid and a fresh
id would give one event two names and break the dedupe every reader depends on.
Each machine exports what it wrote, and the union of the batches is the whole
log. The visible cost is that `qp timeline` shows local history only; the
merged history is in `foreign_event` until a later change teaches the read
commands to union the two.

## The order

`(ts, origin_uuid, origin_id)`, and it is not defined here: the order and the
legality check that keeps it honest both live in `merge`, so that `sync` and
anything else that replays a union derives the sequence the same way. Two copies
of an order that must agree are a divergence waiting for a schedule. All this
command adds is the union of the local log with the peers' events, which is the
part that needs a database handle.

Wall clock alone is not an order. Two stores both hold an event with id 2, and
two events written in the same millisecond on two machines would be sequenced
one way here and the other way there — after which the two stores have quietly
diverged with nothing to detect it. The uuid tiebreak makes the sequence a
function of the set, so every machine that has seen the same batches computes
the same one; the event id behind it keeps a store's own same-millisecond burst
in the order that store wrote it.

Ordering by wall clock is acceptable *because* every transition is then checked,
not because clocks are trusted. See *Anomalies*.

## Identity: which ticket is which

A task's canonical name is the uuid of the store that created it plus the rowid
it holds there. `task_origin` maps that name to the local rowid for every task
this store did not mint; a task with no row there is native, and its canonical
name follows from its own rowid, which is why `qp add` writes nothing.

The map also rides in every exported batch header, and it has to. A peer's
events reference tasks by the peer's rowids — including tasks the peer imported
from us — so without the header's copy a machine cannot tell that the peer's
task 4 is its own task 1, and one ticket merges as two.

**Display ids are local names.** Two machines that have merged the same batches
hold the same tickets with the same fields, edges and history, but may number a
foreign ticket differently, because each mints local rowids above whatever it
had already used. Convergence is a claim about canonical names; `QP-12` is a
label this store put on one of them. Native tickets never renumber, so an id
anyone has already written down keeps pointing at what it always did.

## Concurrent edits

An `edit` event records a field-level delta with the expected prior value —
`{"changes":{"title":{"from":"old","to":"new"}}}` — which is a compare-and-swap
per field. Three cases follow, and only the third is a conflict:

1. **Different fields.** Both apply. Analogous to a merge of different lines.
2. **Same field, sequential.** The second machine had already seen the first's
   value, so the `from` values chain (`A→B` then `B→C`). Applied in order.
3. **Same field, concurrent.** Both edits carry the *same* `from`. Two edits
   branching off one prior state is what concurrency is, and the shared `from`
   is what makes it detectable rather than guessed at.

On case 3 the field is left at the `base` both sides agree on, **both** candidate
values are recorded in the `conflict` table with the events and stores that
produced them, and the task is tagged `needs-resolution`. Later edits chaining
onto either candidate extend that branch instead of touching the field.

Last-write-wins by timestamp is not an option and is not a near miss: clocks on
two machines disagree, so it silently discards a real edit — the exact failure
the format survey rejected in every design it examined. An edit conflict is
recoverable in a way a duplicated *claim* is not; it is two values, and showing
both costs nothing. Detection and marking are this command's job. The verb that
settles a conflict is a separate one.

## Anomalies

Every replayed transition is checked twice, because one check is not enough.

**Continuity** is the same guard the live command ran: one conditional
`UPDATE ... WHERE state = <from>`, and `changes() == 1` decides. It catches a
sequence that arrives out of order — two stores moving one task at once, or a
move whose predecessor is missing.

**Legality** is a question about the event itself: is `from` -> `to` an edge of
the state machine at all? Continuity does not imply it. An event recording
`pending` straight to `assigned` applies perfectly cleanly to a task that *is*
pending, so a continuity-only check writes the hop and reports success — which
is how that defect, found in bulk in pre-fidelity logs, would cross from a
damaged ledger into a healthy store.

Either refusal names the ticket, both ends of the move, both stores involved and
both timestamps. The reader is usually trying to work out whose clock is wrong,
and a message naming only the event that failed sends them to the one store that
is probably fine.

**A merge aborts on any anomaly and writes nothing.** A projection the log
cannot justify is what a silent ordering bug looks like, and folding one into a
store that currently holds good data is the failure this design exists to
prevent. `--force` writes it anyway and still reports everything.

**A restore into an empty store aborts too — unless the ledger has already
declared itself pre-fidelity**, by carrying a `state_change` that records no
origin state. Then it rebuilds as far as the log allows, reports every anomaly
in full, and says on stderr that it did so. There is nothing to protect in that
direction: the target is empty by definition, and refusing leaves someone
mid-recovery with an empty store rather than a partial one. `best_effort` in the
JSON outcome says which happened, so a script wanting a clean rebuild reads that
rather than the exit code.

Which events survive that latitude is decided by the same principle. An event
whose *recorded* origin makes the pair illegal is refused in both modes — the
log is asserting something impossible. An event with no recorded origin is
applied by a restore and refused by a merge: it records where the task went,
which is not in doubt, and only the origin replay had to supply is unreachable.
Against a real pre-fidelity ledger, dropping those instead left most of the
store's finished tickets `pending` in the restored copy — a recovery that looks
complete and reads as though the work was never done.

The line, stated once: legality is absolute wherever the log claims to know
where a move started, and relaxed only where the log itself admits it does not.
A ledger whose events all carry their origins refuses on any anomaly, in both
modes. See `merge` for the reasoning in full.

## History the log cannot rebuild, and why that is permanent

A store written before the event log recorded every write holds three kinds of
gap. They are different problems and import treats them differently, but they
share a cause and none of them can be backfilled: the origin state was never
stored, and a reconstruction would be a guess wearing an audit trail's clothes.

**1. Events that record no origin state.** A pre-fix `state_change` is
`{"title":...,"to":"ready"}` — where it came FROM is absent. The move itself is
not in doubt, only the expectation to check it against, so replay takes the
origin from where it has the task. That is a weaker check, not a guess about
content; it is counted and reported on every import. Refusing outright would
make every store written before that fix permanently unrestorable, turning a
partial loss into a total one.

The derived origin is still checked for legality, and that is where it earns its
keep: a move to `running` on a task nothing ever assigned is refused even though
the event claims no origin to contradict. Events that DO carry a `from` are
checked absolutely, and an illegal one still aborts.

Reading those events naively is worse than it sounds: with no `from`, every
creation looks like a transition and every task looks like one the log never
created. On a real pre-fix store that was very nearly every task in it, and the
whole ledger read as unrestorable. The `title` is what identifies a
creation, because only a creation carries one.

**2. Writes that emitted no event at all.** In that same era, tag, dependency
and description writes recorded nothing. Measured on a real pre-fix store, the
log accounted for only a small fraction of the tag rows present, a majority but
not all of the edges, and almost none of the descriptions. **Nothing can
enumerate this loss** — an event that was never written leaves no trace to
detect — so import warns whenever it sees a ledger of that era and says plainly
that the task count can look right while the store is materially incomplete.

A **restore** of such a ledger therefore rebuilds what the ledger holds and no
more. A **merge** must not compound it, which is what the next section is about.

**3. Tasks with no creation event at all.** Rarer, and the only one that blocks.
Import refuses by default, names the tasks, and `--force` is genuinely best
effort: a restore rebuilds every other task, edge and event and names each event
it had to drop (an event referencing a task that does not exist cannot be
written at all — `event.task_id` is a foreign key), and a forced merge carries
the affected rows across untouched.

Not every unresolvable-looking reference is one of these. An edge written before
the task it points at is an ordering artifact — `qp add --part-of` emits the
containment edge and the new task's creation in one transaction, edge first — so
replay pulls the creation forward rather than failing on it.

## A merge is never destructive

A rebuild is authoritative only over what the log records. Rebuilding a real
store from its log alone would delete every row the pre-fix era never logged —
the large majority of tag rows, on a real store measured against — silently, on
every sync, while reporting success. That is not a partial restore. It is data loss
caused by syncing.

So a merge distinguishes a row the log **deleted** from a row it **never knew
about**. Replay records every `(task, tag)`, `(task, dep)`, `(from, to, kind)`
and `(task, field)` that any event named, in either direction. A row the log
spoke to is the rebuild's business; a row it never named is carried forward
untouched. A logged `tag_removed` still removes, a peer's edit still wins, and
unlogged history survives. The same rule covers `tier`, `description`, and
`state` — for state, only a transition that named its origin gives the log the
authority to move a task the store had already placed.

This is not behind `--force`. Not destroying live data is the default.

## What this command does not do

It does not fetch. `--from` is a directory that already exists, however it got
there — a synced folder, a clone, a copied disk. It does not resolve conflicts,
only detect and mark them. And it does not renumber native tasks, ever.
