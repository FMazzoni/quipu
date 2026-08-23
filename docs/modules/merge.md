The guard that keeps that order honest lives here too: an order is only a
sequence, and a sequence still has to be a legal one.

Replaying one store's log needs no ordering decision: `event.id` is an
AUTOINCREMENT and therefore already a total order over that store's events.
Union-merging the append-only batches of several stores gives a *set*, and a set
does not replay. Two machines holding the same set of events could pick
different orders, rebuild different projections, and nothing would say so —
which silently voids the property the replay-equivalence test exists to
guarantee.

`import` is the consumer today and `sync` will be the next one. The order lives
here rather than beside either of them because two implementations that must
agree are a bug waiting for a schedule: the copy that drifts is the one whose
convergence property then proves nothing.

# The order

`(timestamp, origin store, event id)`, in that priority. It is total and
deterministic — it depends only on the events themselves, never on which machine
ran the merge, in what order the files arrived, or how many times a batch was
seen.

Each key earns its place:

- **timestamp** carries the causal signal. Wall clocks are the only shared
  reference two stores have without a coordination protocol.
- **origin store** breaks a same-instant tie the same way on every machine.
  Without it two hosts can order a tie differently and diverge — rarely, and
  silently, which is the worst combination available.
- **event id** is not redundant. Two events written by the *same* store in the
  same millisecond tie on both earlier keys, and their original store order is
  the only defensible sequence for them. Dropping this key would let a task's
  own `ready` and `assigned` swap places.

Timestamps are compared as canonicalised strings rather than parsed instants, so
the ordering carries no date-library semantics and has no failure mode for a
timestamp nothing can parse. `canonical_ts` pads the fractional-seconds field
first, because a plain string compare puts `…:23.7Z` *after* `…:23.737Z`.

# Why ordering alone is not correctness

A scalar clock orders any pair, but it cannot distinguish "B happened after
observing A" from "B happened concurrently, in ignorance of A". Where two clocks
disagree, a merged order can place a reaction before the thing it reacted to:
`assigned -> running` ahead of the `ready -> assigned` that preceded it.

So replay must **validate**, not merely order, and it does so twice over:

- **continuity** — the task must actually be in the state the event records
  moving from. This is the guarded `UPDATE` the live command wrote, replayed
  verbatim, and it catches an out-of-order sequence.
- **legality** — `from` -> `to` must be an edge of the state machine.
  Continuity alone does not imply it: an event recording `pending -> assigned`
  applies cleanly to a task that *is* pending, and that hop was found in bulk in
  pre-fidelity logs. Merging such a ledger without this check carries the defect
  into a healthy store.

A merge that produces an illegal sequence is a bug report — most often about a
clock — so the refusal names the task, the transition, both source stores and
both timestamps. Prevention where it is cheap, loud detection everywhere else.

# Where refusal is the right answer, and where it is the destructive one

Detection is not the same decision as refusal, and the two modes do not weigh a
refusal the same way.

**A merge refuses.** It folds history into a store holding good data right now,
and the store a bad merge damages is the one in front of you. Refusing costs a
sync; accepting costs the data.

**A restore into an empty store does not**, when the ledger has already declared
itself pre-fidelity — some `state_change` in it records no origin state at all.
There is nothing to protect: the target is empty by definition, and refusing
hands someone mid-recovery an empty store instead of a partial one. It rebuilds
as far as the log allows and reports every gap.

The same asymmetry decides what happens to the offending event itself:

- **A recorded origin** makes the illegal pair the *log's own claim*, and the
  claim is impossible. Refused in both modes. Nothing recovers from a log
  asserting a move the machine cannot make.
- **A derived origin** — no `from` at all, because the binary that wrote it
  predates the log recording origins — is a different accusation. The event says
  where the task *went*, and that is not in doubt; only the origin replay had to
  supply is unreachable. A merge still refuses it. A restore applies it, because
  discarding it is not the cautious choice: measured against a real pre-fidelity
  ledger, dropping these moves left most of the store's finished tickets sitting
  in `pending` in the recovered copy. The destination is evidence. The missing
  origin is the gap, and the gap is reported either way.

So legality is enforced absolutely wherever the log claims to know where a move
started, and relaxed only where the log itself admits it does not. A modern
ledger that produces anomalies still refuses in both modes.

`--force` is not the mechanism for any of this, deliberately. A flag every real
recovery has to pass is a flag people learn to pass always, which spends the
guard's credibility everywhere else to buy it in one place.

# "Pre-fidelity" is a property of the writing binary, not of a date

There is no clean cutoff to detect. A stale binary in a worktree writes
legacy-shaped events *today*: a single store's log has been observed carrying
origin-less events and origin-carrying events written within the same hour, by
two binaries of different vintage. So nothing here keys off a version or a
timestamp — the detector asks each event whether it recorded an origin, which is
the only question that has a reliable answer.

The same conditions produce a gap no guard can close: where the binary that
should have written a promotion never wrote one, replay stands in `pending`
while the next event records leaving `ready`, and no ordering repairs a
transition that was never recorded. That is reported as an anomaly naming the
ticket, which is the most any replayer can do about it.

# What this deliberately is not

Not a hybrid logical clock and not a causal-parent pointer. Both were
considered; both were deferred in favour of a scalar timestamp plus this guard,
which is most of the benefit for a fraction of the work and is *measurable*. If
the guard never fires across months of real use, the theoretical hole was never
a practical one; if it does, there is a concrete failure to design against
rather than a speculation. The upgrade path stays open — events still carry
timestamps, so a richer clock can be added later without invalidating data
written under this scheme.

# The hole this does not close

The guard is a state-machine guard, and only state transitions have a state
machine. Tags, dependency edges and relations are add/remove pairs with no
illegal sequence: if a skewed clock sorts a peer's `tag_removed` before the
`tag_added` it was reacting to, replay applies both, in that order, and the tag
survives — correctly, by the order it was handed, and wrongly by causality.
Nothing here can detect that, because nothing in a scalar timestamp distinguishes
it from two machines that genuinely disagreed. Field edits are the exception:
they record their own `from` value, which is why `import` can diff them and mark
a conflict.

# Boundaries

Pure functions over strings and integers: no database handle, no store schema,
no `crate::` references. Ordering and legality are the whole of it. What a
refusal *does* — abort, roll back, report under `--force` — belongs to the
consumer, and the identity of the tasks being ordered (two stores both number a
task 4) belongs to whatever builds the merged stream.
